use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::queue::mutation::{
    session_request_create_mutation, SessionRequestFields,
};
use gents_session::request::lifecycle::queue::*;
use gents_session::request::lifecycle::test_support::test_db;
use gents_session::request::lifecycle::test_support::{hints, wake_queue_input};
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_store::identity::NodeIdentity;

use gents_automation::goal::continuation::{goal_continuation_identity, prepare_goal_continuation};
use gents_session::request::lifecycle::RequestTerminalOutcome;
use gents_session::session;
use gents_toolcalls::test_support::background_completion::{
    admission_verifier, background_hints, desktop_owned_session,
};

use gents_toolcalls::test_support::queue::background_completion::*;
use gents_toolcalls::test_support::queue::*;

#[tokio::test]
async fn background_wakes_and_goal_continuations_keep_the_project_directory() {
    let db = test_db("continuation-project-directory").await;
    let project = db._tempdir.path().join("project with spaces 工具");
    std::fs::create_dir(&project).unwrap();
    let cwd = project
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut fixture = canonical_background_fixture_with_input(
        &db,
        "project-session",
        &RequestInput {
            cwd: Some(cwd.clone()),
            ..Default::default()
        },
    )
    .await;
    let enqueued = fixture
        .persist_notification(
            "background command finished",
            "background-completion-notification:project:tool",
            "review results",
            background_hints(&fixture.parent),
            None,
        )
        .await
        .unwrap();
    let wake = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &db.agent_access,
        &enqueued.request.unwrap().doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wake.input.cwd.as_deref(), Some(cwd.as_str()));
    let goal = prepare_goal_continuation(
        &wake,
        db.agent_did().into(),
        "goal",
        "continue",
        1,
        false,
        "2030-01-01T00:00:01Z",
        wake.request_hop,
    )
    .unwrap();
    assert_eq!(goal.input.cwd.as_deref(), Some(cwd.as_str()));
}

#[tokio::test]
async fn desktop_session_runtime_controls_adopt_owner_and_reject_foreign_ancestry() {
    for source in [
        QueueSource::BackgroundCompletion,
        QueueSource::Steering,
        QueueSource::Goal,
    ] {
        let db = test_db("desktop-control-scope").await;
        let session_id = "desktop-owned-session";
        let (desktop, create, parent, writer) = desktop_owned_session(&db, session_id).await;
        let query = format!(
            "{{ AgentSession {{ {} }} }}",
            gents_protocol::session::AGENT_SESSION_FIELDS
        );
        let before = agent_data(&db, &query).await;
        let mutation = if source == QueueSource::Goal {
            let mut continuation = prepare_goal_continuation(
                &parent,
                db.agent_did().into(),
                "goal",
                "continue",
                1,
                false,
                "2030-01-01T00:00:01Z",
                parent.request_hop,
            )
            .unwrap();
            gents_session::request::admission::sign_agent_request_create(
                db.agent_identity.as_ref(),
                &mut continuation,
            )
            .await
            .unwrap();
            continuation.graphql_mutation().unwrap()
        } else {
            session_request_create_mutation(
                &parent,
                SessionRequestFields {
                    agent_did: db.agent_did(),
                    content: "continue",
                    execution_origin: ExecutionOrigin::Scheduled,
                    input: RequestInput {
                        queue: Some(hints(source, QueuePolicy::Append)),
                        ..Default::default()
                    },
                    request_id: "runtime-control",
                    created_at: "2030-01-01T00:00:01Z",
                    retry_key: None,
                },
                &db.agent_identity.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap()
        };
        agent_write(&db, "test.desktop_control_request", &mutation).await;
        let request_id = if source == QueueSource::Goal {
            goal_continuation_identity("goal", &parent.request_id, 1)
                .unwrap()
                .request_id
        } else {
            "runtime-control".into()
        };
        let request = gents_session::request::binding::load_agent_request_with_access(
            &db.agent_access,
            &request_id,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(request.requester_did.as_deref(), Some(desktop.did()));
        let mut lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                db.node.clone(),
                request.clone(),
                60,
                ExecutionOrigin::Scheduled,
                "backend-test",
            );
        lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        let sessions = agent_data(&db, &query).await;
        assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
        assert_eq!(
            sessions["AgentSession"][0]["_docID"], before["AgentSession"][0]["_docID"],
            "control must reuse the desktop-owned session"
        );
        assert_eq!(sessions["AgentSession"][0]["requester_did"], desktop.did());
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();

        let next = session_request_create_mutation(
            &request,
            SessionRequestFields {
                agent_did: db.agent_did(),
                content: "second-hop",
                execution_origin: ExecutionOrigin::Scheduled,
                input: wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
                request_id: "second-control",
                created_at: "2030-01-01T00:00:02Z",
                retry_key: None,
            },
            &db.agent_identity.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        agent_write(&db, "test.desktop_second_control", &next).await;
        let second = gents_session::request::binding::load_agent_request_with_access(
            &db.agent_access,
            "second-control",
        )
        .await
        .unwrap()
        .unwrap();
        let mut second_lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                db.node.clone(),
                second,
                60,
                ExecutionOrigin::Scheduled,
                "backend-test",
            );
        second_lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        assert_eq!(
            second_lifecycle.claim_with_identity().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        assert_eq!(
            agent_data(&db, &query).await["AgentSession"][0]["_docID"],
            before["AgentSession"][0]["_docID"]
        );
        second_lifecycle
            .begin_owned_execution(&writer)
            .await
            .unwrap();
        second_lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();

        let forged = session_request_create_mutation(
            &request,
            SessionRequestFields {
                agent_did: db.agent_did(),
                content: "signed-original",
                execution_origin: ExecutionOrigin::Scheduled,
                input: wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
                request_id: "forged-control",
                created_at: "2030-01-01T00:00:03Z",
                retry_key: None,
            },
            &db.agent_identity.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap()
        .replace("signed-original", "unsigned-tampering");
        agent_write(&db, "test.desktop_forged_control", &forged).await;
        let forged = gents_session::request::binding::load_agent_request_with_access(
            &db.agent_access,
            "forged-control",
        )
        .await
        .unwrap()
        .unwrap();
        let error = admission_verifier(&db)
            .verify_fresh(&forged, db.agent_did())
            .await
            .unwrap_err();
        assert!(error.is_denied(), "{error:#}");
        let mut forged_lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                db.node.clone(),
                forged,
                60,
                ExecutionOrigin::Scheduled,
                "backend-test",
            );
        forged_lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        forged_lifecycle
            .reject_admission(&error.to_string())
            .await
            .unwrap();
        let failed = agent_data(
            &db,
            "{ AgentRequest(filter: { request_id: { _eq: \"forged-control\" } }) { lifecycle_state failure_reason } }",
        )
        .await;
        assert_eq!(failed["AgentRequest"][0]["lifecycle_state"], "failed");
        assert_eq!(
            failed["AgentRequest"][0]["failure_reason"],
            error.to_string()
        );

        let mut followup = create.clone();
        followup.initial_lifecycle_state = RequestLifecycleState::Pending;
        followup.request_id = "desktop-followup".into();
        followup.retry_root_request = Some(followup.request_id.clone());
        followup.created_at = "2030-01-01T00:00:04Z".into();
        followup.content = "is it finished?".into();
        gents_session::request::admission::sign_agent_request_create(&desktop, &mut followup)
            .await
            .unwrap();
        agent_write(
            &db,
            "test.desktop_followup",
            &followup.graphql_mutation().unwrap(),
        )
        .await;
        let followup = gents_session::request::binding::load_agent_request_with_access(
            &db.agent_access,
            "desktop-followup",
        )
        .await
        .unwrap()
        .unwrap();
        let mut followup_lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                db.node.clone(),
                followup,
                60,
                ExecutionOrigin::Interactive,
                "backend-test",
            );
        followup_lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        assert_eq!(
            followup_lifecycle.claim_with_identity().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        let after = agent_data(&db, &query).await;
        assert_eq!(after["AgentSession"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["AgentSession"][0]["_docID"],
            before["AgentSession"][0]["_docID"]
        );
        assert_eq!(after["AgentSession"][0]["requester_did"], desktop.did());

        let mut foreign_parent = parent.clone();
        foreign_parent.session_id = "other-session".into();
        let bad = session_request_create_mutation(
            &foreign_parent,
            SessionRequestFields {
                agent_did: db.agent_did(),
                content: "bad",
                execution_origin: ExecutionOrigin::Scheduled,
                input: wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
                request_id: "foreign-control",
                created_at: "2030-01-01T00:00:02Z",
                retry_key: None,
            },
            &db.agent_identity.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        agent_write(&db, "test.desktop_foreign_control", &bad).await;
        // Agent-owned, so the ensure sees the session the Agent's own write may
        // already have created instead of colliding with it.
        let desktop_did = desktop.did().to_owned();
        let node_did = db.node_did().to_owned();
        let agent_did = db.agent_did().to_owned();
        let now = chrono::Utc::now().to_rfc3339();
        db.agent_access
            .transact_write("test.foreign_control_session", |txn| {
                let (desktop_did, node_did, agent_did, now) = (
                    desktop_did.as_str(),
                    node_did.as_str(),
                    agent_did.as_str(),
                    now.as_str(),
                );
                Box::pin(async move {
                    session::ensure_session_in_txn(
                        txn,
                        "other-session",
                        node_did,
                        agent_did,
                        Some(desktop_did),
                        None,
                        None,
                        now,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let bad = gents_session::request::binding::load_agent_request_with_access(
            &db.agent_access,
            "foreign-control",
        )
        .await
        .unwrap()
        .unwrap();
        let error = admission_verifier(&db)
            .verify_fresh(&bad, db.agent_did())
            .await
            .unwrap_err();
        assert!(error.is_denied(), "{error:#}");
        assert!(
            error.to_string().contains("outside its session"),
            "{error:#}"
        );
    }
}
