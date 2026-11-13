use super::*;

const SESSION: &str = "delivery-goal-existing-session";
const INITIAL: &str = "delivery-goal-existing-request";
const OLD_OBJECTIVE: &str =
    "Keep working on the current request; never complete this old objective.";
const GOAL_WORK_SDL: &str =
    "type DeliveryGoalWork { handoff_id: String @immutable kind: String @immutable session_id: String @immutable }";

async fn evidence(access: &ConfigAccess, owner: &str) -> Result<Value> {
    let owner = escape_graphql_string(owner);
    let query = format!(
        r#"{{
        AgentRequest(filter:{{node_did:{{_eq:"{owner}"}}}}){{_docID request_id session_id lifecycle_state caused_by_trigger_kind failure_reason}}
        Trigger(filter:{{node_did:{{_eq:"{owner}"}}}}){{trigger_id last_status last_error}}
        TriggerFire(filter:{{owner_did:{{_eq:"{owner}"}}}}){{fire_key trigger_id request_id session_id goal_id}}
        FireOutcome(filter:{{owner_did:{{_eq:"{owner}"}}}}){{fire_key trigger_id request_id session_id goal_id terminal_state created_at}}
        Goal(filter:{{node_did:{{_eq:"{owner}"}}}}){{goal_id session_id objective status assignment_root_request_doc_id}}
        InferenceCall(filter:{{node_did:{{_eq:"{owner}"}}}}){{request_id call_kind call_state started_at ended_at}}
        AgentToolCall(filter:{{session_id:{{_eq:"{SESSION}"}}}}){{request_id tool_name lifecycle_state}}
    }}"#
    );
    Ok(access
        .transact_readonly("test.delivery_goal.snapshot", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?)
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

async fn submit(access: &ConfigAccess, kind: &str) -> Result<()> {
    create_source(
        access,
        "DeliveryGoalWork",
        GOAL_WORK_SDL,
        json!({"handoff_id": format!("goal-{kind}"), "kind": kind, "session_id": SESSION}),
    )
    .await?;
    Ok(())
}

struct Gate {
    address: std::net::SocketAddr,
    entered: Arc<tokio::sync::Notify>,
    released: tokio::sync::watch::Sender<bool>,
    listener: tokio::task::JoinHandle<()>,
}

impl Gate {
    async fn new() -> Result<Self> {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let (released, receiver) = tokio::sync::watch::channel(false);
        let listener = tokio::spawn(async move {
            while let Ok((mut connection, _)) = listener.accept().await {
                notify.notify_one();
                let mut receiver = receiver.clone();
                tokio::spawn(async move {
                    let ready = receiver.wait_for(|released| *released).await.is_ok();
                    if ready {
                        let _ = connection.write_all(b"r").await;
                    }
                });
            }
        });
        Ok(Self {
            address,
            entered,
            released,
            listener,
        })
    }

    fn arguments(&self) -> Value {
        json!({"command":format!(
            "python3 -c 'import socket; s=socket.create_connection((\"127.0.0.1\", {})); s.recv(1); print(\"gate released\")'",
            self.address.port(),
        )})
    }

    async fn wait(&self, deadline: tokio::time::Instant) -> Result<()> {
        tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(120)),
            self.entered.notified(),
        )
        .await
        .context("live model did not enter its requested host tool")
    }

    fn release(&self) {
        self.released.send_replace(true);
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
        self.listener.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "uses local inference; set GENTS_LIVE_TARGET and GENTS_DELIVERY_SOURCE_SHA"]
async fn goal_task_waits_for_claim_and_emits_only_after_model_completion() -> Result<()> {
    let lanes = delivery_lanes().await?;
    let artifacts = tokio::task::spawn_blocking(|| artifact_directory("goal")).await??;
    let home =
        gents::runtime::embedded::EmbeddedHome::create_retained(&artifacts.join("home")).await?;
    let db = support::test_db_from_home(home);
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let owner = identity.did().to_owned();
    let (fixture, agents) = configure(&db, &lanes).await?;
    let access = fixture.access.clone();
    install_schema(&access, GOAL_WORK_SDL).await?;
    let initial = Gate::new().await?;
    let continuation = Gate::new().await?;
    let objective = format!(
        "This is a Goal continuation. Call bash_unrestricted once with exactly {}. After that tool returns, call update_goal with status complete and a brief reason. Then acknowledge briefly. Do not create another Goal.",
        continuation.arguments(),
    );
    support::fixtures::configure_agent_tools(
        &db, &owner, &agents.lead,
        Some("Follow the current request and Goal objective. Initial Task deliveries and inbox replies must not call update_goal. On a durable Goal controller continuation, perform its objective and complete it only after its requested host tool returns. Do not create another Goal or repeat a completed host tool call.".into()),
        gents_documents::Tools {
            tools_id: format!("{}:tools", agents.lead), node_did: owner.clone(),
            host: Some(gents_documents::HostTools {
                root: Some(artifacts.display().to_string()),
                bash: Some(gents_documents::BashTools { mode: gents_documents::BashMode::Unrestricted, ..Default::default() }),
                ..Default::default()
            }),
            built_ins: Some(gents_documents::BuiltInTools { enable_goal_tools: Some(true), ..Default::default() }),
            ..Default::default()
        }, Vec::new(),
    ).await;
    let mut documents = Vec::new();
    for (kind, emit, assignment, prompt) in [
        ("goal", true, Some(objective.as_str()), "This is the initial Task delivery for your new Goal, not a controller continuation. Do not call update_goal or host tools in this request. Acknowledge briefly."),
        ("reply", false, None, "This is an inbox reply, not a controller continuation. Do not call update_goal or host tools. Acknowledge briefly."),
    ] {
        let id = format!("delivery-goal-{kind}");
        documents.extend([
            (Collection::Task, json!({"task_id":id,"agent_did":&agents.lead,"emit_outcome":emit,"goal_objective_template":assignment,"prompt_template":prompt})),
            (Collection::EventSource, json!({"event_source_id":id,"source_collection":"DeliveryGoalWork","event_kind":"created","filter":format!("{{kind:{{_eq:\"{}\"}}}}", escape_graphql_string(kind)) })),
            (Collection::Trigger, json!({"trigger_id":id,"task_id":id,"enabled":true,"concurrency":"queued_serial","session_id_template":"{{ doc.session_id }}","source":{"kind":"event","event_source_id":id}})),
        ]);
    }
    apply(&access, &owner, documents).await?;
    let runtime = support::live_inference::boot_live_agent_with_ceiling(
        &db,
        identity,
        gents_tools::surface::ToolCeiling::readwrite(&artifacts),
    )
    .await?;
    let result: Result<()> = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
        let mut changes = DeliveryChanges::new(&access).await;
        support::agent_requests::submit_user_request(&owner, &fixture, SESSION, INITIAL,
            &format!("Call bash_unrestricted once with exactly {}. After the tool returns, acknowledge briefly. Do not call update_goal or any other tool.", initial.arguments()),
            gents_protocol::request_input::QueueDelivery::Queue,
        ).await;
        gents_automation::goal::seed_goal_from_access(&access, &owner, SESSION, Some(OLD_OBJECTIVE),
            Some(gents_protocol::goal_status::GoalStatus::Active), None, None,
        ).await?;
        initial.wait(deadline).await?;
        submit(&access, "goal").await?;
        let queued = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            if table(&state, "TriggerFire").iter().any(|row| row["trigger_id"] == "delivery-goal-goal") { break state; }
            changes.recv(deadline).await?;
        };
        let fire = table(&queued, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-goal").unwrap();
        ensure!(table(&queued, "AgentRequest").iter().any(|row| row["request_id"] == fire["request_id"] && row["lifecycle_state"] == "pending"), "Goal Task did not queue behind the active request: {queued}");
        ensure!(table(&queued, "AgentRequest").iter().any(|row| row["request_id"] == INITIAL && row["lifecycle_state"] == "processing"), "the initial request stopped before its gate was released: {queued}");
        ensure!(table(&queued, "Goal").iter().any(|row| row["objective"] == OLD_OBJECTIVE && row["status"] == "active"), "busy session objective changed: {queued}");
        ensure!(fire["session_id"] == SESSION, "Goal Task routed to another session: {queued}");
        let request_id = string(fire, "request_id")?.to_owned();
        let fire_key = string(fire, "fire_key")?.to_owned();
        initial.release();
        continuation.wait(deadline).await?;
        let continuing = evidence(&access, &owner).await?;
        healthy(&continuing)?;
        ensure!(table(&continuing, "FireOutcome").is_empty(), "Goal emitted before model completion: {continuing}");
        ensure!(table(&continuing, "AgentRequest").iter().any(|row| row["request_id"] == request_id && row["lifecycle_state"] == "completed"), "initial Goal Task did not finish before its continuation: {continuing}");
        ensure!(table(&continuing, "Goal").iter().any(|row| row["objective"] == objective && row["status"] == "active"), "queued assignment missing at claim: {continuing}");
        let root = table(&continuing, "AgentRequest").iter().find(|row| row["request_id"] == request_id).context("Goal Task request missing")?;
        ensure!(table(&continuing, "Goal").iter().any(|row| row["assignment_root_request_doc_id"] == root["_docID"]), "Goal assignment names another request: {continuing}");
        submit(&access, "reply").await?;
        let queued_reply = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            if let Some(reply) = table(&state, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-reply") {
                let request = table(&state, "AgentRequest").iter().find(|row| row["request_id"] == reply["request_id"]).context("reply request missing")?;
                ensure!(request["lifecycle_state"] == "pending", "reply did not queue behind the continuation: {state}");
                break state;
            }
            changes.recv(deadline).await?;
        };
        continuation.release();
        let complete = loop {
            let state = evidence(&access, &owner).await?;
            healthy(&state)?;
            let outcomes = table(&state, "FireOutcome");
            ensure!(outcomes.len() <= 1, "duplicate or chained Goal outcome: {state}");
            let replied = table(&state, "TriggerFire").iter().find(|row| row["trigger_id"] == "delivery-goal-reply").is_some_and(|fire|
                table(&state, "AgentRequest").iter().any(|row| row["request_id"] == fire["request_id"] && row["lifecycle_state"] == "completed"));
            if outcomes.len() == 1 && replied {
                ensure!(outcomes[0]["fire_key"] == fire_key && outcomes[0]["terminal_state"] == "complete", "wrong Goal outcome: {state}");
                ensure!(table(&state, "Goal").iter().any(|row| row["status"] == "complete"), "outcome preceded Goal completion: {state}");
                break state;
            }
            changes.recv(deadline).await?;
        };
        tokio::fs::write(artifacts.join("goal-evidence.json"), serde_json::to_vec_pretty(&json!({
            "queued_assignment":queued,"ordinary_boundary":continuing,"queued_reply":queued_reply,"completed":complete
        }))?).await?;
        Ok(())
    }.await;
    if let Ok(state) = evidence(&access, &owner).await {
        tokio::fs::write(
            artifacts.join("goal-final-state.json"),
            serde_json::to_vec_pretty(&state)?,
        )
        .await?;
    }
    runtime.shutdown().await;
    db.node.shutdown().await?;
    result
}
