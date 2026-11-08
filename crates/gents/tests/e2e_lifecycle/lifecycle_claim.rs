use gents::request::lifecycle::ClaimOutcome;
use gents::request::watcher::{AgentRequest, DefraWatcher};
use gents::Watcher as _;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::support::{first_row, test_db};

type StatusRow = AgentRequestRow;

type DeadlineRow = AgentRequestRow;

#[tokio::test]
async fn claim_queues_when_earlier_processing_request_exists() {
    let db = test_db("lifecycle-dedup").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new("req-earlier", &session_id, &earlier)
                .state("processing"),
        )
        .await;

    let later = (chrono::Utc::now() + chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new("req-later", &session_id, &later)
                .state("pending"),
        )
        .await;
    let request = AgentRequest {
        content: "second".into(),
        ..actors.build_request(doc_id, "req-later".into(), session_id, later)
    };

    let mut lifecycle = actors.lifecycle(
        request,
        300,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Queued);

    let resp = actors
        .query(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Pending));
}

#[tokio::test]
async fn queued_request_interrupt_wins_before_queue_block() {
    let db = test_db("lifecycle-queued-interrupt").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = "2026-03-23T00:00:00Z";

    actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                "req-earlier-active",
                &session_id,
                earlier,
            )
            .state("processing"),
        )
        .await;

    let later = "2026-03-23T00:00:01Z";
    let doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                "req-later-interrupted",
                &session_id,
                later,
            )
            .state("pending"),
        )
        .await;
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    actors
        .set_interrupt_requested_at(&doc_id, &interrupt_at)
        .await;

    let request = AgentRequest {
        content: "second".into(),
        ..actors.build_request(
            doc_id,
            "req-later-interrupted".into(),
            session_id,
            later.into(),
        )
    };

    let mut lifecycle = actors.lifecycle(
        request,
        300,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);

    let resp = actors
        .query(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later-interrupted" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
}

#[tokio::test]
async fn queued_request_valid_until_wins_before_queue_block() {
    let db = test_db("lifecycle-queued-valid-until").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = "2026-03-23T00:00:00Z";

    actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                "req-earlier-active",
                &session_id,
                earlier,
            )
            .state("processing"),
        )
        .await;

    let later = "2026-03-23T00:00:01Z";
    let expired_at = (chrono::Utc::now() - chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                "req-later-expired",
                &session_id,
                later,
            )
            .state("pending")
            .valid_until(Some(&expired_at)),
        )
        .await;

    let request = AgentRequest {
        content: "second".into(),
        ..actors.build_request(doc_id, "req-later-expired".into(), session_id, later.into())
    };

    let mut lifecycle = actors.lifecycle(
        request,
        300,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);

    let resp = actors
        .query(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later-expired" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Dead));
}
#[tokio::test]
async fn same_timestamp_queue_order_uses_request_id_tie_break() {
    let db = test_db("lifecycle-same-timestamp-order").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = "2026-03-23T00:00:00Z";
    let first_doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new("req-a", &session_id, created_at)
                .state("pending"),
        )
        .await;
    let second_doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new("req-b", &session_id, created_at)
                .state("pending"),
        )
        .await;

    let second_request = AgentRequest {
        content: "second".into(),
        ..actors.build_request(
            second_doc_id,
            "req-b".into(),
            session_id.clone(),
            created_at.into(),
        )
    };
    let mut second_lifecycle = actors.lifecycle(
        second_request,
        300,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(
        second_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );

    let first_request = AgentRequest {
        content: "first".into(),
        ..actors.build_request(first_doc_id, "req-a".into(), session_id, created_at.into())
    };
    let mut first_lifecycle = actors.lifecycle(
        first_request,
        300,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(
        first_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );
}
/// Every request writer is an admission owner (P2P carries no requests), and
/// the client owner reserves the session in the same transaction that would
/// switch its Agent, so the pin is refused there before any row exists.
#[tokio::test]
async fn agent_change_is_rejected_without_mutating_the_session() {
    use crate::support::accepted_turn::enqueue_managed_accepted_request;
    use gents::NodeIdentity as _;
    let db = test_db("lifecycle-agent-pin").await;
    // The User opens the session with its first Agent, then addresses a
    // second Agent it enrolled on the same Node under that session label.
    let pinned = crate::support::managed_agent_fixture(&db).await;
    let switched = crate::support::managed_agent_fixture(&db).await;
    enqueue_managed_accepted_request(&db, &pinned, "req-open", "session-pinned", "open").await;

    let mut switch = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "req-switch",
        db.node_identity.did(),
        switched.user.did(),
        &switched.binding.agent_did,
        "session-pinned",
        "switch agent",
        "interactive",
        crate::support::agent_requests::canonical_now(),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(switched.user.did()),
    );
    let error = gents::request::lifecycle::reserve_goal_session(&switched.access, &switch)
        .await
        .expect_err("agent switch must fail");
    let error = gents_protocol::error_chain::ErrorChain(&error).to_string();
    assert!(
        error.contains("client request changes selected session agent"),
        "{error}"
    );
    // A row naming the pinned session cannot publish either.
    let pinned_session = pinned
        .access
        .execute(
            r#"{ AgentSession(filter: { session_id: { _eq: "session-pinned" } }) { _docID } }"#,
        )
        .await
        .expect("read session as its User");
    switch.session_doc_id = pinned_session["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .map(str::to_owned);
    let node_did = db.node_identity.did();
    let switched_agent = switched.binding.agent_did.as_str();
    switch.agent_doc_id = Some(
        switched
            .access
            .transact_readonly("test.agent_pin.agent", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
                            node_did,
                            switched_agent,
                        )
                        .await?
                        .expect("switched Agent")
                        .0,
                    )
                })
            })
            .await
            .expect("read switched Agent"),
    );
    gents::sign_agent_request_create(&switched.user, &mut switch)
        .await
        .expect("sign switch as the User");
    let error = gents::request::lifecycle::submit_client_request(&switched.access, &switch)
        .await
        .expect_err("publishing an agent switch must fail");
    let error = gents_protocol::error_chain::ErrorChain(&error).to_string();
    assert!(
        error.contains("client request changes selected session agent"),
        "{error}"
    );

    let requests = switched
        .access
        .execute(r#"{ AgentRequest(filter: { request_id: { _eq: "req-switch" } }) { _docID } }"#)
        .await
        .expect("read requests as the User");
    assert_eq!(
        requests["data"]["AgentRequest"].as_array().map(Vec::len),
        Some(0),
        "a refused switch leaves no request row"
    );
    let sessions = pinned
        .access
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "session-pinned" } }) { agent_did }
            }"#,
        )
        .await
        .expect("read session as its User");
    let rows = sessions["data"]["AgentSession"]
        .as_array()
        .expect("session rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("agent_did").and_then(serde_json::Value::as_str),
        Some(pinned.binding.agent_did.as_str())
    );
}
#[tokio::test]
async fn claim_synthesizes_deadline_when_request_deadline_is_invalid() {
    let db = test_db("lifecycle-invalid-deadline").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let request_id = "req-invalid-deadline";
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = crate::support::agent_requests::canonical_now();
    let invalid_deadline = "not-a-deadline";
    let doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;
    actors
        .update_request(&doc_id, serde_json::json!({"deadline": invalid_deadline}))
        .await;

    let mut watcher =
        DefraWatcher::new(db.node.clone(), actors.node_did()).expect("request watcher");
    watcher
        .set_agent_identities(vec![actors.agent().clone()])
        .expect("watcher Agent identity");
    let request = watcher
        .try_fetch_request(&doc_id)
        .await
        .unwrap()
        .expect("pending request");
    assert_eq!(request.deadline.as_deref(), Some(invalid_deadline));

    let before_claim = chrono::Utc::now();
    let mut lifecycle = actors.lifecycle(
        request,
        120,
        gents::request::lifecycle::ExecutionOrigin::Interactive,
        "",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let after_claim = chrono::Utc::now();

    let resp = actors
        .query(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-invalid-deadline" } },
                    limit: 1
                ) { request_id deadline }
            }"#,
        )
        .await;
    let persisted = first_row::<DeadlineRow>(&resp, "AgentRequest")
        .deadline
        .expect("AgentRequest.deadline");
    assert_ne!(persisted, invalid_deadline);

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&persisted)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(persisted_deadline >= before_claim + chrono::Duration::seconds(120));
    assert!(persisted_deadline <= after_claim + chrono::Duration::seconds(121));
}
