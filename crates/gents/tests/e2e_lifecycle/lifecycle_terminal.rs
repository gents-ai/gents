use gents_session::request::lifecycle::ClaimOutcome;
use gents_session::request::lifecycle::RequestTerminalOutcome;

use crate::support::{first_row, test_db};

#[tokio::test]
async fn missing_session_observation_does_not_block_terminal_request() {
    let db = test_db("lifecycle-missing-terminal-projection").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let session_id = "session-missing-terminal-projection";
    let request_id = "req-missing-terminal-projection";
    let request_doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                request_id,
                session_id,
                "2026-03-23T00:00:00Z",
            )
            .state("pending"),
        )
        .await;
    let request = actors.build_request(
        request_doc_id,
        request_id.to_string(),
        session_id.to_string(),
        "2026-03-23T00:00:00Z".to_string(),
    );
    let mut lifecycle = actors.lifecycle(
        request,
        300,
        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let response = db
        .node
        .execute(
            r#"mutation {
                delete_AgentSession(
                    filter: { session_id: { _eq: "session-missing-terminal-projection" } }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let response = actors
        .query(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-missing-terminal-projection" } }
                ) { lifecycle_state }
            }"#,
        )
        .await;
    let request = first_row::<serde_json::Value>(&response, "AgentRequest");
    assert_eq!(request["lifecycle_state"], "completed");
}

#[tokio::test]
async fn stale_completion_leaves_the_newer_session_observation_as_modelled() {
    let cases = gents_lean_vocab::families::session_stale_completion_observation::cases::<String>();
    for (index, case) in cases.iter().enumerate() {
        let db = test_db(&format!("lifecycle-stale-complete-{index}")).await;
        let actors = crate::support::agent_requests::RequestActors::new(&db).await;
        let session_id = "session-stale";
        let first_doc_id = actors
            .create_request(
                crate::support::agent_requests::RequestSeed::new(
                    "req-1",
                    session_id,
                    "2026-03-23T00:00:00Z",
                )
                .state("pending"),
            )
            .await;
        let first_request = actors.build_request(
            first_doc_id,
            "req-1".into(),
            session_id.into(),
            "2026-03-23T00:00:00Z".into(),
        );
        let mut lifecycle = actors.lifecycle(
            first_request,
            300,
            gents_session::request::lifecycle::ExecutionOrigin::Interactive,
            "",
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

        actors
            .create_request(
                crate::support::agent_requests::RequestSeed::new(
                    "req-2",
                    session_id,
                    "2026-03-23T00:00:01Z",
                )
                .state(&case.input.newer_state),
            )
            .await;
        actors
            .seed_session_observation_from_request(session_id, "req-2", "second")
            .await;

        crate::support::begin_owned_execution(&mut lifecycle, &db.node)
            .await
            .unwrap();
        let outcome = match case.input.stale_state.as_str() {
            "completed" => RequestTerminalOutcome::Completed,
            "failed" => RequestTerminalOutcome::Failed,
            other => unreachable!("stale request terminal state {other}"),
        };
        lifecycle
            .terminalize_owned(
                outcome,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();

        let session = crate::support::snapshots::fetch_session_snapshot(&actors, session_id)
            .await
            .expect("session");
        let latest = session
            .observation
            .expect("observation")
            .latest_request
            .expect("latest request");
        case.check(
            gents_lean_vocab::families::session_stale_completion_observation::StaleOutput {
                latest_request_id: latest
                    .request_id
                    .strip_prefix("req-")
                    .map(|number| number.parse().expect("numeric request id")),
                latest_state: Some(latest.lifecycle_state.as_str().to_string()),
            },
        );
    }
}
