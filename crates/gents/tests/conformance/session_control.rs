use super::*;

pub(super) async fn seed_agent_session(
    db: &support::TestDb,
    fixture: &support::ManagedAgentFixture,
    session_id: &str,
    created_at: &str,
) -> (gents::ConfigAccess, String, String) {
    let access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        fixture.agent.did().parse().unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let agent_doc = fixture
        .access
        .transact_readonly("test.session_control.agent", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::Agent,
                        db.node_identity.did(),
                        fixture.agent.did(),
                    )
                    .await?
                    .expect("fixture Agent")
                    .0,
                )
            })
        })
        .await
        .unwrap();
    let session_doc = access.transact_write("test.session_control.session", |txn| {
        let agent_doc = &agent_doc;
        Box::pin(async move {
            let input = serde_json::json!({"node_did":db.node_identity.did(),
                "session_id":session_id,"requester_did":fixture.agent.did(),
                "agent_did":fixture.agent.did(),"created_at":created_at,
                "relationships_pending":false});
            let response = txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                &serde_json::json!({"input":input}),
            ).await?;
            let session_doc = gents::store::graphql::created_doc_id(&response,"AgentSession")?;
            txn.add_resource_relationship("AgentSession", &session_doc, "parent", "agents", agent_doc).await?;
            txn.add_actor_relationship("AgentSession", &session_doc, "working_agent", &fixture.agent.did().parse()?).await?;
            Ok::<_,anyhow::Error>(session_doc)
        })
    }).await.unwrap();
    (access, agent_doc, session_doc)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the signed fixture binds a request to its physical session and Agent"
)]
pub(super) async fn seed_agent_request(
    db: &support::TestDb,
    fixture: &support::ManagedAgentFixture,
    access: &gents::ConfigAccess,
    agent_doc: &str,
    session_doc: &str,
    session_id: &str,
    request_id: &str,
    created_at: &str,
) -> String {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        db.node_identity.did(),
        fixture.agent.did(),
        fixture.agent.did(),
        session_id,
        "session control witness",
        "interactive",
        created_at,
        AgentRequestAdmissionRecord::local_self(fixture.agent.did()),
    );
    create.agent_doc_id = Some(agent_doc.to_owned());
    create.session_doc_id = Some(session_doc.to_owned());
    gents::sign_agent_request_create(fixture.agent.as_ref(), &mut create)
        .await
        .unwrap();
    access
        .transact_write("test.session_control.request", |txn| {
            let create = &create;
            Box::pin(async move {
                let response = txn
                    .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&response, "AgentRequest")?;
                txn.add_resource_relationship("AgentRequest", &doc, "parent", "agents", agent_doc)
                    .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &doc,
                    "session",
                    "sessions",
                    session_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &fixture.agent.did().parse()?,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap()
}

async fn load_goal(
    access: &gents::ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> gents::goal::GoalDocument {
    access
        .transact_readonly("test.session_control.goal", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::goal::load_canonical_goal_in_txn(txn, node_did, session_id)
                        .await?
                        .expect("goal"),
                )
            })
        })
        .await
        .unwrap()
}

fn model_case(name: &str) -> serde_json::Value {
    lean_session_control_cases()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("Lean session-control model omitted {name}"))
        .clone()
}

#[tokio::test]
async fn generated_session_control_cases_drive_native_interrupt_stop_and_resume() {
    let interrupt_case = model_case("interrupt_idle");
    let stop_case = model_case("stop_drains");
    let resume_case = model_case("resume_terminal_goal");
    let db = support::test_db("session-control-generated").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let node_did = db.node_identity.did();

    let interrupted_session = uuid::Uuid::new_v4().to_string();
    assert_eq!(interrupt_case["initial_pending_count"], 0);
    let (access, agent_doc, _) =
        seed_agent_session(&db, &fixture, &interrupted_session, "2026-10-20T10:00:00Z").await;
    gents::goal::set_goal_from_access(
        &access,
        node_did,
        &interrupted_session,
        Some("pause on session interrupt"),
        Some(gents::goal::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents::session::message::interrupt_session_with_access(
            &db.node,
            &access,
            &gents::session::origin::SessionScope {
                node_did: node_did.to_owned(),
                session_id: interrupted_session.clone(),
                requester_did: Some(fixture.agent.did().to_owned()),
            },
        )
        .await
        .unwrap(),
        None
    );
    let interrupted_goal = load_goal(&access, node_did, &interrupted_session).await;
    assert_eq!(
        interrupted_goal.status,
        interrupt_case["goal_status"].as_str().unwrap()
    );
    let interrupted_session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&interrupted_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
    assert_eq!(
        interrupted_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
        interrupt_case["stopped"].as_bool().unwrap()
    );

    let stopped_session = uuid::Uuid::new_v4().to_string();
    let (_, _, stopped_session_doc) =
        seed_agent_session(&db, &fixture, &stopped_session, "2026-10-20T10:00:01Z").await;
    let mut pending_ids = Vec::new();
    let mut pending_docs = Vec::new();
    for index in 0..stop_case["initial_pending_count"].as_u64().unwrap() {
        let pending_id = uuid::Uuid::new_v4().to_string();
        let pending_doc = seed_agent_request(
            &db,
            &fixture,
            &access,
            &agent_doc,
            &stopped_session_doc,
            &stopped_session,
            &pending_id,
            &format!("2026-10-20T10:00:{:02}Z", 2 + index),
        )
        .await;
        pending_ids.push(pending_id);
        pending_docs.push(pending_doc);
    }
    gents::goal::set_goal_from_access(
        &access,
        node_did,
        &stopped_session,
        Some("stop and later resume"),
        Some(gents::goal::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let receipt = gents::session::stop_session_with_access(
        &db.node,
        &access,
        node_did,
        &stopped_session,
        Some(fixture.agent.did()),
    )
    .await
    .unwrap();
    assert_eq!(receipt.drained_request_ids, pending_ids);
    let stopped_session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
    assert_eq!(
        stopped_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
        stop_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        load_goal(&access, node_did, &stopped_session).await.status,
        stop_case["goal_status"].as_str().unwrap()
    );

    for pending_doc in pending_docs {
        let lifecycle = access.execute(&format!(
            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 1) {{lifecycle_state}}}}"#,
            escape_graphql_string(&pending_doc)
        ))
        .await;
        assert_eq!(
            lifecycle.unwrap()["data"]["AgentRequest"][0]["lifecycle_state"],
            "interrupted"
        );
    }
    let pending_rows = access.execute(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}, lifecycle_state: {{_eq: "pending"}}}}, limit: 2) {{request_id}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
    assert_eq!(
        pending_rows.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        stop_case["pending_empty"].as_bool().unwrap()
    );
    gents::goal::set_goal_from_access(
        &access,
        node_did,
        &stopped_session,
        None,
        Some(gents::goal::GoalStatus::Complete),
        None,
        None,
    )
    .await
    .unwrap();
    let job_home = tempfile::tempdir().unwrap();
    let job_driver = gents::tools::background::job::driver::JobDriver::new(
        gents::tools::background::job::driver::JobRuntime::new(
            db.node.clone(),
            std::sync::Arc::new(gents::plugin::executor::PluginExecutor::new(None)),
            job_home.path().to_path_buf(),
            None,
        ),
    );
    gents::session::resume_session_with_access(
        &db.node,
        &access,
        node_did,
        &stopped_session,
        Some(fixture.agent.did()),
        gents::tools::call::hook::BackgroundExecutionRegistry::default(),
        job_driver,
    )
    .await
    .unwrap();
    let resumed_session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
    assert_eq!(
        resumed_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
        resume_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        load_goal(&access, node_did, &stopped_session).await.status,
        resume_case["goal_status"].as_str().unwrap()
    );
    db.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn generated_resume_after_settlement_stages_the_paused_goal_continuation() {
    let resume_case = model_case("resume_after_settlement");
    let db = support::test_db("session-control-resume").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let node_did = db.node_identity.did();
    let session_id = uuid::Uuid::new_v4().to_string();
    let (access, agent_doc, session_doc) =
        seed_agent_session(&db, &fixture, &session_id, "2026-10-20T11:00:00Z").await;
    let predecessor_id = uuid::Uuid::new_v4().to_string();
    let predecessor_doc = seed_agent_request(
        &db,
        &fixture,
        &access,
        &agent_doc,
        &session_doc,
        &session_id,
        &predecessor_id,
        "2026-10-20T11:00:01Z",
    )
    .await;
    access.write("test.session_control.processing", &format!(
        "mutation {{ update_AgentRequest(docID: \"{}\", input: {{lifecycle_state: \"processing\"}}) {{_docID}} }}",
        escape_graphql_string(&predecessor_doc),
    )).await.unwrap();
    gents::goal::set_goal_from_access(
        &access,
        node_did,
        &session_id,
        Some("resume after the stopped turn settles"),
        Some(gents::goal::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let receipt = gents::session::stop_session_with_access(
        &db.node,
        &access,
        node_did,
        &session_id,
        Some(fixture.agent.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt.interrupted_request_id.as_deref(),
        Some(predecessor_id.as_str())
    );

    let job_home = tempfile::tempdir().unwrap();
    let job_driver = gents::tools::background::job::driver::JobDriver::new(
        gents::tools::background::job::driver::JobRuntime::new(
            db.node.clone(),
            std::sync::Arc::new(gents::plugin::executor::PluginExecutor::new(None)),
            job_home.path().to_path_buf(),
            None,
        ),
    );
    let resume = || {
        gents::session::resume_session_with_access(
            &db.node,
            &access,
            node_did,
            &session_id,
            Some(fixture.agent.did()),
            gents::tools::call::hook::BackgroundExecutionRegistry::default(),
            job_driver.clone(),
        )
    };
    assert_eq!(
        resume().await.is_err(),
        resume_case["refused_while_active"].as_bool().unwrap(),
        "resume must wait for the interrupted request to settle"
    );

    access.write("test.session_control.interrupted", &format!(
        "mutation {{ update_AgentRequest(docID: \"{}\", input: {{lifecycle_state: \"interrupted\"}}) {{_docID}} }}",
        escape_graphql_string(&predecessor_doc),
    )).await.unwrap();
    resume().await.unwrap();

    let session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&session_id)
    )).await;
    assert_eq!(
        session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
        resume_case["stopped"].as_bool().unwrap()
    );
    let goal = load_goal(&access, node_did, &session_id).await;
    assert_eq!(goal.status, resume_case["goal_status"].as_str().unwrap());
    assert_eq!(
        goal.last_continued_from_request_id.as_deref(),
        Some(predecessor_id.as_str())
    );
    let continuation = access.execute(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}, lifecycle_state: {{_eq: "pending"}}}}, limit: 2) {{request_id}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&session_id)
    )).await;
    assert_eq!(
        continuation.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "resume stages exactly one signed continuation"
    );
    db.node.shutdown().await.expect("node shutdown");
}
