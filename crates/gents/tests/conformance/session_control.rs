use super::*;

pub(super) async fn seed_agent_session(
    db: &support::TestDb,
    fixture: &support::ManagedAgentFixture,
    session_id: &str,
    created_at: &str,
) -> (gents_store::config_client::ConfigAccess, String, String) {
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
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
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
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
            let session_doc = gents_store::graphql::created_doc_id(&response,"AgentSession")?;
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
    access: &gents_store::config_client::ConfigAccess,
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
    gents_session::request::admission::sign_agent_request_create(
        fixture.agent.as_ref(),
        &mut create,
    )
    .await
    .unwrap();
    access
        .transact_write("test.session_control.request", |txn| {
            let create = &create;
            Box::pin(async move {
                let response = txn
                    .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> gents_automation::goal::GoalDocument {
    access
        .transact_readonly("test.session_control.goal", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_automation::goal::load_canonical_goal_in_txn(txn, node_did, session_id)
                        .await?
                        .expect("goal"),
                )
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn generated_session_control_cases_drive_native_interrupt_stop_and_resume() {
    let db = support::test_db("session-control-generated").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let node_did = db.node_identity.did();

    let mut agent_doc = None;
    for interrupt_case in
        gents_lean_vocab::families::session_control_contracts_interrupt_idle::cases()
    {
        let interrupted_session = uuid::Uuid::new_v4().to_string();
        assert_eq!(interrupt_case.input.initial_pending_count, 0);
        let (access, seeded_agent_doc, _) =
            seed_agent_session(&db, &fixture, &interrupted_session, "2026-10-20T10:00:00Z").await;
        agent_doc = Some((access.clone(), seeded_agent_doc));
        gents_automation::goal::seed_goal_from_access(
            &access,
            node_did,
            &interrupted_session,
            Some("pause on session interrupt"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            gents_session::session::message::interrupt_session_with_access(
                &db.node,
                &access,
                &gents_protocol::session::SessionScope {
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
        let interrupted_session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&interrupted_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
        interrupt_case.check(
            gents_lean_vocab::families::session_control_contracts_interrupt_idle::GoalOutput {
                goal_status: interrupted_goal.status.to_string(),
                stopped: interrupted_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"]
                    .is_string(),
            },
        );
    }
    let (access, agent_doc) = agent_doc.expect("the interrupt case seeds the Agent session");

    for stop_case in gents_lean_vocab::families::session_control_contracts_stop_drains::cases() {
        let stopped_session = uuid::Uuid::new_v4().to_string();
        let (_, _, stopped_session_doc) =
            seed_agent_session(&db, &fixture, &stopped_session, "2026-10-20T10:00:01Z").await;
        let mut pending_ids = Vec::new();
        let mut pending_docs = Vec::new();
        for index in 0..stop_case.input.initial_pending_count {
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
        gents_automation::goal::seed_goal_from_access(
            &access,
            node_did,
            &stopped_session,
            Some("stop and later resume"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let receipt = gents_session::session::stop_session_with_access(
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
        let stopped =
            stopped_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string();
        let stopped_goal_status = load_goal(&access, node_did, &stopped_session).await.status;

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
        stop_case.check(
            gents_lean_vocab::families::session_control_contracts_stop_drains::DrainOutput {
                goal_status: stopped_goal_status.to_string(),
                stopped,
                pending_empty: pending_rows.unwrap()["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            },
        );
        for resume_case in
            gents_lean_vocab::families::session_control_contracts_resume_terminal_goal::cases()
        {
            assert_eq!(resume_case.input.name, "resume_terminal_goal");
            gents_automation::goal::seed_goal_from_access(
                &access,
                node_did,
                &stopped_session,
                None,
                Some(gents_protocol::goal_status::GoalStatus::Complete),
                None,
                None,
            )
            .await
            .unwrap();
            let job_home = tempfile::tempdir().unwrap();
            let job_driver = gents_toolcalls::background::job::driver::JobDriver::new(
                gents_toolcalls::background::job::driver::JobRuntime::new(
                    db.node.clone(),
                    std::sync::Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
                        None,
                    )),
                    job_home.path().to_path_buf(),
                    None,
                )
                .with_session_goals(std::sync::Arc::new(
                    gents_automation::goal::session_message::SessionGoals,
                )),
            );
            gents_session::session::resume_session_with_access(
                &db.node,
                &access,
                node_did,
                &stopped_session,
                Some(fixture.agent.did()),
                std::sync::Arc::new(gents_toolcalls::call::hook::RuntimeExecutionWitness {
                    background_executions:
                        gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
                    job_driver,
                }),
            )
            .await
            .unwrap();
            let resumed_session_row = access.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session),
        escape_graphql_string(fixture.agent.did())
    )).await;
            resume_case.check(gents_lean_vocab::families::session_control_contracts_resume_terminal_goal::GoalOutput {
        goal_status: load_goal(&access, node_did, &stopped_session)
            .await
            .status
            .to_string(),
        stopped: resumed_session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
    });
        }
    }
    db.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn generated_resume_after_settlement_stages_the_paused_goal_continuation() {
    for resume_case in
        gents_lean_vocab::families::session_control_contracts_resume_after_settlement::cases()
    {
        assert_eq!(resume_case.input.name, "resume_after_settlement");
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
        gents_automation::goal::seed_goal_from_access(
            &access,
            node_did,
            &session_id,
            Some("resume after the stopped turn settles"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let receipt = gents_session::session::stop_session_with_access(
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
        let job_driver = gents_toolcalls::background::job::driver::JobDriver::new(
            gents_toolcalls::background::job::driver::JobRuntime::new(
                db.node.clone(),
                std::sync::Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(None)),
                job_home.path().to_path_buf(),
                None,
            )
            .with_session_goals(std::sync::Arc::new(
                gents_automation::goal::session_message::SessionGoals,
            )),
        );
        let resume = || {
            gents_session::session::resume_session_with_access(
                &db.node,
                &access,
                node_did,
                &session_id,
                Some(fixture.agent.did()),
                std::sync::Arc::new(gents_toolcalls::call::hook::RuntimeExecutionWitness {
                    background_executions:
                        gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
                    job_driver: job_driver.clone(),
                }),
            )
        };
        let refused_while_active = resume().await.is_err();

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
        let goal = load_goal(&access, node_did, &session_id).await;
        resume_case.check(
        gents_lean_vocab::families::session_control_contracts_resume_after_settlement::ResumeOutput {
            goal_status: goal.status.to_string(),
            stopped: session_row.unwrap()["data"]["AgentSession"][0]["stopped_at"].is_string(),
            refused_while_active,
        },
    );
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
}
