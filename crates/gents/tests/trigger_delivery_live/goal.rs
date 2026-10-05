use super::*;

const SESSION: &str = "delivery-goal-existing-session";
const INITIAL: &str = "delivery-goal-existing-request";
const OLD_OBJECTIVE: &str =
    "Keep working on the current request; never complete this old objective.";
const OBJECTIVE: &str = "On an explicit durable goal controller continuation, first write a numbered checklist with 100 short entries, then call update_goal with status complete and reason 'Goal Task continuation verified', and reply GOAL_TASK_COMPLETE. The initial Task delivery and inbox replies must not complete the Goal.";

async fn evidence(access: &ConfigAccess, owner: &str) -> Result<Value> {
    let owner = escape_graphql_string(owner);
    let query = format!(
        r#"{{
        AgentRequest(filter:{{agent_did:{{_eq:"{owner}"}}}}){{_docID request_id session_id lifecycle_state caused_by_trigger_kind failure_reason}}
        Trigger(filter:{{agent_did:{{_eq:"{owner}"}}}}){{trigger_id last_status last_error}}
        TriggerFire(filter:{{owner_did:{{_eq:"{owner}"}}}}){{fire_key trigger_id request_id session_id goal_id goal_assignment_applied}}
        FireOutcome(filter:{{owner_did:{{_eq:"{owner}"}}}}){{fire_key trigger_id request_id session_id goal_id terminal_state created_at}}
        Goal(filter:{{agent_did:{{_eq:"{owner}"}}}}){{goal_id session_id objective status assignment_root_request_doc_id}}
        InferenceCall(filter:{{agent_did:{{_eq:"{owner}"}}}}){{request_id call_kind call_state started_at ended_at}}
        AgentToolCall(filter:{{session_id:{{_eq:"{SESSION}"}}}}){{request_id tool_name lifecycle_state}}
    }}"#
    );
    access
        .transact("test.delivery_goal.snapshot", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await
}

fn table<'a>(state: &'a Value, name: &str) -> &'a [Value] {
    state["data"][name]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn healthy(state: &Value) -> Result<()> {
    ensure!(
        table(state, "AgentRequest").iter().all(|row| {
            !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                row["lifecycle_state"].as_str(),
            ) || row["lifecycle_state"] == "completed"
        }),
        "live Goal request failed: {state}"
    );
    ensure!(
        table(state, "Trigger")
            .iter()
            .all(|row| row["last_status"] != "error"),
        "live Goal trigger admission failed: {state}"
    );
    Ok(())
}

fn inference_running(state: &Value, request: &str) -> bool {
    table(state, "InferenceCall").iter().any(|row| {
        row["request_id"] == request
            && row["call_kind"] == "inference"
            && row["call_state"] == "running"
    })
}

async fn submit(access: &ConfigAccess, kind: &str) -> Result<()> {
    access
        .write(
            "test.delivery_goal.submit",
            &format!(
        "mutation {{create_DeliveryGoalWork(input:{{handoff_id:\"{}\",kind:\"{}\",session_id:\"{}\"}}){{_docID}}}}",
        escape_graphql_string(&format!("goal-{kind}")), escape_graphql_string(kind), escape_graphql_string(SESSION),
    ),
        )
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real inference: GENTS_TRIGGER_DELIVERY_LIVE=1 and workstation endpoints"]
async fn goal_task_waits_for_claim_and_emits_only_after_model_completion() -> Result<()> {
    ensure!(
        std::env::var("GENTS_TRIGGER_DELIVERY_LIVE").as_deref() == Ok("1"),
        "explicit live opt-in required"
    );
    let endpoints = [
        std::env::var("GENTS_DELIVERY_ENDPOINT_1")?,
        std::env::var("GENTS_DELIVERY_ENDPOINT_2")?,
    ];
    let artifacts = artifact_directory("goal")?;
    let home =
        gents::eval::runner::embedded::EmbeddedHome::create_retained(&artifacts.join("home"))
            .await?;
    let db = support::test_db_from_home(home);
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let owner = identity.did().to_owned();
    let access = ConfigAccess::Local(db.node.clone());
    access.add_schema(SCHEMA).await?;
    access
        .add_schema(
            "type DeliveryGoalWork { handoff_id: String @immutable kind: String @immutable session_id: String @immutable }",
        )
        .await?;
    configure(&access, &db.node, &owner, &endpoints).await?;
    support::fixtures::configure_behavior_tools(
        &db.node, &owner, "delivery-lead",
        Some("Follow the current request and Goal objective. Initial Task deliveries and inbox replies must not call update_goal. On an explicit durable Goal controller continuation, perform the objective and call update_goal complete when instructed. Do not create another Goal or use unrelated tools.".into()),
        gents::document_config::Tools {
            tools_id: "delivery-lead:tools".into(), agent_did: owner.clone(),
            built_ins: Some(gents::document_config::BuiltInTools {
                enable_goal_tools: Some(true), ..Default::default()
            }),
            ..Default::default()
        }, Vec::new(),
    ).await;
    let mut documents = Vec::new();
    for (kind, emit, objective, prompt) in [
        ("goal", true, Some(OBJECTIVE), "This is the initial Task delivery for your new Goal, not a controller continuation. Do not call update_goal in this request. Reply exactly GOAL_TASK_PROGRESS."),
        ("reply", false, None, "This is an inbox reply, not a controller continuation. Do not call update_goal. Reply exactly GOAL_INBOX_REPLY."),
    ] {
        let id = format!("delivery-goal-{kind}");
        documents.extend([
            (Collection::Task, json!({"task_id":id,"behavior_id":"delivery-lead", "emit_outcome":emit,
                "goal_objective_template":objective,"prompt_template":prompt})),
            (Collection::EventSource, json!({"event_source_id":id,"source_collection":"DeliveryGoalWork",
                "event_kind":"created","filter":format!("{{kind:{{_eq:\"{kind}\"}}}}")})),
            (Collection::Trigger, json!({"trigger_id":id,"task_id":id,"enabled":true,"concurrency":"queued_serial",
                "session_id_template":"{{ doc.session_id }}","source":{"kind":"event","event_source_id":id}})),
        ]);
    }
    apply(&access, &owner, documents).await?;
    let runtime = boot_live_agent(&db, identity).await?;
    let result: Result<()> = async {
    support::interrupt::create_runtime_request(&db.node, &owner, "delivery-lead", INITIAL, SESSION,
        "Write a numbered list with 150 entries, each saying 'still working'. Do not call tools and do not complete the Goal.").await;
    gents::goal::set_goal_from_access(
        &access,
        &owner,
        SESSION,
        Some(OLD_OBJECTIVE),
        Some(gents::goal::GoalStatus::Active),
        None,
        None,
    )
    .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
        loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            if inference_running(&state, INITIAL) { break; }
            ensure!(!table(&state, "AgentRequest").iter().any(|row|
                row["request_id"] == INITIAL && row["lifecycle_state"] == "completed"),
                "initial inference ended before its busy window was observed: {state}");
            ensure!(tokio::time::Instant::now() < deadline, "initial real inference never started: {state}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        submit(&access, "goal").await?;
        let queued = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            if table(&state, "TriggerFire").iter().any(|row| row["trigger_id"] == "delivery-goal-goal") { break state; }
            ensure!(tokio::time::Instant::now() < deadline, "Goal Task admission timed out: {state}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let fire = table(&queued, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-goal").unwrap();
        ensure!(fire["goal_assignment_applied"] == false, "Goal assignment took effect before queued claim: {queued}");
        ensure!(table(&queued, "Goal").iter().any(|row| row["objective"] == OLD_OBJECTIVE && row["status"] == "active"), "busy session objective changed: {queued}");
        ensure!(table(&queued, "AgentRequest").iter().any(|row| row["request_id"] == INITIAL && row["lifecycle_state"] == "processing"), "assignment was not observed behind a busy request: {queued}");
        ensure!(fire["session_id"] == SESSION, "Goal Task routed to a different session: {queued}");
        let request_id = string(fire, "request_id")?.to_owned();
        let fire_key = string(fire, "fire_key")?.to_owned();
        let continuing = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            ensure!(table(&state, "FireOutcome").is_empty(), "Goal emitted before observable continuation boundary: {state}");
            let initial_complete = table(&state, "AgentRequest").iter().any(|row| row["request_id"] == request_id && row["lifecycle_state"] == "completed");
            let child_running = table(&state, "AgentRequest").iter().any(|row|
                row["caused_by_trigger_kind"] == "goal" && row["request_id"] != INITIAL &&
                row["request_id"].as_str().is_some_and(|id| inference_running(&state, id)));
            if initial_complete && child_running { break state; }
            ensure!(tokio::time::Instant::now() < deadline, "Goal continuation not observed: {state}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        ensure!(table(&continuing, "Goal").iter().any(|row| row["objective"] == OBJECTIVE && row["status"] == "active"), "queued assignment missing at claim: {continuing}");
        ensure!(table(&continuing, "TriggerFire").iter().any(|row| row["fire_key"] == fire_key && row["goal_assignment_applied"] == true), "claim omitted assignment witness: {continuing}");
        let root = table(&continuing, "AgentRequest").iter().find(|row| row["request_id"] == request_id).context("Goal Task root missing")?;
        ensure!(table(&continuing, "Goal").iter().any(|goal| goal["assignment_root_request_doc_id"] == root["_docID"]), "Goal assignment points at a different request: {continuing}");
        submit(&access, "reply").await?;
        let queued_reply = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            if let Some(reply) = table(&state, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-reply") {
                let request = table(&state, "AgentRequest").iter().find(|row| row["request_id"] == reply["request_id"]).context("reply request missing")?;
                ensure!(request["lifecycle_state"] == "pending", "reply did not queue behind continuation: {state}");
                break state;
            }
            ensure!(tokio::time::Instant::now() < deadline, "reply admission timed out: {state}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let complete = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            let outcomes = table(&state, "FireOutcome");
            ensure!(outcomes.len() <= 1, "duplicate or chained Goal outcomes: {state}");
            let replied = table(&state, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-reply").is_some_and(|fire|
                table(&state, "AgentRequest").iter().any(|row| row["request_id"] == fire["request_id"] && row["lifecycle_state"] == "completed"));
            if outcomes.len() == 1 && replied {
                ensure!(outcomes[0]["fire_key"] == fire_key && outcomes[0]["terminal_state"] == "complete", "wrong Goal outcome: {state}");
                ensure!(table(&state, "Goal").iter().any(|row| row["status"] == "complete"), "outcome preceded Goal completion: {state}");
                ensure!(table(&state, "AgentToolCall").iter().any(|row| row["tool_name"] == "update_goal" && row["lifecycle_state"] == "completed"), "completion did not use real model tool: {state}");
                break state;
            }
            ensure!(tokio::time::Instant::now() < deadline, "Goal outcome did not settle: {state}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        std::fs::write(artifacts.join("goal-evidence.json"), serde_json::to_vec_pretty(&json!({
            "queued_assignment":queued,"ordinary_boundary":continuing,"queued_reply":queued_reply,"completed":complete
        }))?)?;
        Ok(())
    }.await;
    if let Ok(state) = evidence(&access, &owner).await {
        std::fs::write(
            artifacts.join("goal-final-state.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
    }
    runtime.shutdown().await;
    db.node.shutdown().await;
    result
}
