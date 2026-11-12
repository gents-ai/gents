use super::{
    admission_fixture::publish_accepted_on_claimed_request,
    admission_fixture::published_admission_with_owner,
    admission_fixture::PublishedAdmissionOptions, delivery::RawPresentation,
    delivery::TerminalFields, AwaitMode,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use gents_protocol::{output::PayloadPresentation, session::SessionObservation};
use gents_session::session::tool_call_state::ToolCallState;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

type Family = gents_lean_vocab::families::contracts_tool_activity::ToolActivityInput<String>;
type Observation = gents_lean_vocab::families::contracts_tool_activity::ObservationWire<String>;

fn observation(
    value: &Observation,
    event_doc_id: u64,
    request_doc_id: &str,
    request_id: &str,
    epoch: DateTime<Utc>,
) -> SessionObservation {
    let latest = value.latest.as_ref().unwrap();
    SessionObservation {
        reply_preview: None,
        last_activity_at: (epoch + Duration::seconds(i64::try_from(value.activity).unwrap()))
            .to_rfc3339_opts(SecondsFormat::Nanos, true),
        preview: value.preview.clone(),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: if latest.doc_id == event_doc_id {
                request_doc_id.to_owned()
            } else {
                format!("model-request-{}", latest.doc_id)
            },
            request_id: request_id.to_owned(),
            lifecycle_state: serde_json::from_value(Value::String(latest.state.clone())).unwrap(),
        }),
    }
}

fn modeled_observation(
    value: &SessionObservation,
    case: &Family,
    request_doc_id: &str,
    request_id: &str,
    epoch: DateTime<Utc>,
) -> Observation {
    let latest = value.latest_request.as_ref().unwrap();
    assert_eq!(latest.request_id, request_id);
    gents_lean_vocab::families::contracts_tool_activity::ObservationWire {
        activity: u64::try_from(
            (DateTime::parse_from_rfc3339(&value.last_activity_at)
                .unwrap()
                .with_timezone(&Utc)
                - epoch)
                .num_seconds(),
        )
        .unwrap(),
        preview: value.preview.clone(),
        latest: Some(
            gents_lean_vocab::families::contracts_tool_activity::LatestWire {
                doc_id: if latest.request_doc_id == request_doc_id {
                    case.event.doc_id
                } else {
                    latest
                        .request_doc_id
                        .strip_prefix("model-request-")
                        .unwrap()
                        .parse()
                        .unwrap()
                },
                request_id: case.event.request_id,
                state: serde_json::to_value(latest.lifecycle_state)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            },
        ),
    }
}

#[tokio::test]
async fn tool_publications_follow_lean_session_activity() {
    for case in gents_lean_vocab::families::contracts_tool_activity::cases::<String>() {
        let input = &case.input;
        let name = input.name.as_str();
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
        let published = input.published;
        if !published {
            admission.tool.complete("ok").await.unwrap();
        }
        let before = observation(
            input.before.observation.as_ref().unwrap(),
            input.event.doc_id,
            &request_doc_id,
            &request_id,
            epoch,
        );
        let session = escape_graphql_string(&session_id);
        // The session's Agent owns it; ACP hides it from any other writer.
        let requester = admission.tool.requester_did().map(str::to_owned);
        let before_session = admission.tool.access().unwrap().transact_write("test.seed_tool_activity", |txn| {
            let before = before.clone();
            let session = session.clone();
            let session_id = session_id.clone();
            let agent = admission.node_did.clone();
            let requester = requester.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    &format!(r#"mutation($input: AgentSessionMutationInputArg!) {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, input: $input) {{ _docID }} }}"#),
                    &serde_json::json!({"input":{"observation":before}}),
                ).await?;
                Ok::<_, anyhow::Error>(gents_store::config_client::agent_session::load_agent_session_row_in_txn(txn, &agent, &session_id, requester.as_deref())
                    .await?.unwrap().session)
            })
        }).await.unwrap();
        let mut steps = Vec::new();
        for turn in 0..2usize {
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
            let step_now = input.now + turn as u64;
            let now = epoch + Duration::seconds(i64::try_from(step_now).unwrap());
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
                    RawPresentation {
                        raw: "ok",
                        rendered: "ok",
                        presentation: PayloadPresentation::Full,
                    },
                    "test.tool_activity",
                    now,
                )
                .await
                .unwrap();
            assert_eq!(changed, published, "{name} turn {turn}");
            let after = admission.tool.access().unwrap().transact_readonly(
                "test.read_tool_activity",
                |txn| {
                    let agent = admission.node_did.clone();
                    let session_id = session_id.clone();
                    let requester = requester.clone();
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>(
                            gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                                txn,
                                &agent,
                                &session_id,
                                requester.as_deref(),
                            )
                            .await?
                            .unwrap()
                            .session,
                        )
                    })
                },
            )
            .await
            .unwrap();
            let mut unchanged = after.clone();
            unchanged.observation = before_session.observation.clone();
            assert_eq!(unchanged, before_session, "{name} turn {turn}");
            let mut modeled = input.before.clone();
            modeled.observation = Some(modeled_observation(
                after.observation.as_ref().unwrap(),
                input,
                &request_doc_id,
                &request_id,
                epoch,
            ));
            steps.push(
                gents_lean_vocab::families::contracts_tool_activity::ToolActivityStep {
                    now: step_now,
                    after: modeled,
                },
            );
        }
        case.check(
            gents_lean_vocab::families::contracts_tool_activity::ToolActivityOutput { steps },
        );
        drop(owner);
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).unwrap();
    }
}
