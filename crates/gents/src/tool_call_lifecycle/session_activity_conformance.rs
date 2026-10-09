use super::{
    admission_fixture::{
        publish_accepted_on_claimed_request, published_admission_with_owner,
        PublishedAdmissionOptions,
    },
    delivery::TerminalFields,
    AwaitMode, ToolCallState,
};
use crate::{config_client::ConfigAccess, graphql::escape_graphql_string};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use gents_protocol::{output::PayloadPresentation, session::SessionObservation};
use serde_json::Value;

fn observation(
    value: &Value,
    event: &Value,
    request_doc_id: &str,
    request_id: &str,
    epoch: DateTime<Utc>,
) -> SessionObservation {
    let latest = &value["latest"];
    SessionObservation {
        last_activity_at: (epoch + Duration::seconds(value["activity"].as_i64().unwrap()))
            .to_rfc3339_opts(SecondsFormat::Nanos, true),
        preview: value["preview"].as_str().map(str::to_owned),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: if latest["doc_id"] == event["doc_id"] {
                request_doc_id.to_owned()
            } else {
                format!("model-request-{}", latest["doc_id"])
            },
            request_id: request_id.to_owned(),
            lifecycle_state: serde_json::from_value(latest["state"].clone()).unwrap(),
        }),
    }
}

#[tokio::test]
async fn tool_publications_follow_lean_session_activity() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection;
    let cases: Vec<_> = cases
        .iter()
        .filter(|case| case["operation"] == "tool_activity")
        .collect();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let (mut admission, mut owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: format!("session-activity-{name}"),
                tool_name: Some("test_activity".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let request = owner.request();
        let request_doc_id = request.doc_id.clone();
        let request_id = request.request_id.clone();
        let session_id = admission.tool.session_id.clone();
        let epoch = Utc::now();
        let published = case["published"].as_bool().unwrap();
        if !published {
            admission.tool.complete("ok").await.unwrap();
        }
        let before = observation(
            &case["before"]["observation"],
            &case["event"],
            &request_doc_id,
            &request_id,
            epoch,
        );
        let session = escape_graphql_string(&session_id);
        let before_session = ConfigAccess::transact_local(&admission.node, None, "test.seed_tool_activity", |txn| {
            let before = before.clone();
            let session = session.clone();
            let session_id = session_id.clone();
            let agent = admission.node_did.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    &format!(r#"mutation($input: AgentSessionMutationInputArg!) {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, input: $input) {{ _docID }} }}"#),
                    &serde_json::json!({"input":{"observation":before}}),
                ).await?;
                Ok(crate::session::load_agent_session_row_in_txn(txn, &agent, &session_id, None)
                    .await?.unwrap().session)
            })
        }).await.unwrap();
        for (turn, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            if published && turn > 0 {
                admission.tool = publish_accepted_on_claimed_request(
                    admission.node.clone(),
                    &mut owner,
                    &admission.node_did,
                    turn,
                    "test_activity",
                    &format!("tool-activity-{turn}"),
                    serde_json::json!({}),
                    AwaitMode::Foreground,
                    true,
                )
                .await
                .unwrap();
            }
            let now = epoch + Duration::seconds(step["now"].as_i64().unwrap());
            let changed = admission
                .tool
                .terminalize_raw_with_presentation_at(
                    ToolCallState::Running,
                    TerminalFields {
                        state: ToolCallState::Completed,
                        failure: None,
                        cancel: None,
                        completion_reason: None,
                    },
                    "ok",
                    "ok",
                    PayloadPresentation::Full,
                    "test.tool_activity",
                    now,
                )
                .await
                .unwrap();
            assert_eq!(changed, published, "{name} turn {turn}");
            let after = ConfigAccess::transact_local(
                &admission.node,
                None,
                "test.read_tool_activity",
                |txn| {
                    let agent = admission.node_did.clone();
                    let session_id = session_id.clone();
                    Box::pin(async move {
                        Ok(crate::session::load_agent_session_row_in_txn(
                            txn,
                            &agent,
                            &session_id,
                            None,
                        )
                        .await?
                        .unwrap()
                        .session)
                    })
                },
            )
            .await
            .unwrap();
            let expected = observation(
                &step["after"]["observation"],
                &case["event"],
                &request_doc_id,
                &request_id,
                epoch,
            );
            let mut expected_session = before_session.clone();
            expected_session.observation = Some(expected);
            assert_eq!(after, expected_session, "{name} turn {turn}");
        }
        drop(owner);
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).unwrap();
    }
}
