use super::*;
use crate::store::config_client::{count_statements, ConfigAccess, StatementCounts};
use crate::store::graphql::escape_graphql_string;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::Value;

/// One `get_goal` reaches the session's request rows once: the attribution
/// load's own scan. Both the selection and the usage scan read every signed
/// request body, so a second scan is the session's largest repeated transfer.
fn session_request_scans(counts: &StatementCounts) -> usize {
    counts
        .statements()
        .filter(|(label, document)| {
            matches!(
                *label,
                Some("goal.tool_context") | Some("goal.session_usage")
            ) && document.contains("runtime_source_kind")
                && document.contains("session_id")
        })
        .count()
}

async fn goal_commits(access: &ConfigAccess) -> usize {
    access
        .execute("{Goal(showDeleted: true) {_docID _version {cid}}}")
        .await
        .unwrap()["data"]["Goal"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["_version"].as_array().map(Vec::len).unwrap_or_default())
        .sum()
}

async fn goal_tokens_used(access: &ConfigAccess, session: &str) -> i64 {
    access
        .execute(&format!(
            r#"{{Goal(filter: {{session_id: {{_eq: "{}"}}}}) {{tokens_used}}}}"#,
            escape_graphql_string(session)
        ))
        .await
        .unwrap()["data"]["Goal"][0]["tokens_used"]
        .as_i64()
        .unwrap()
}

struct GoalLatencySession {
    node: Arc<EmbeddedNode>,
    identity: KeyIdentity,
    hook: DefraSessionHook,
    session: String,
    caller_doc_id: String,
    session_doc_id: String,
    agent_did: String,
    agent_signer: crate::store::identity::ManagedAgentSigner,
    access: ConfigAccess,
    data_path: std::path::PathBuf,
    call: u64,
    _identity_dir: tempfile::TempDir,
}

impl GoalLatencySession {
    /// `requests` extra signed session requests, each carrying a realistic
    /// prompt body, and `calls` inference calls charged against them.
    async fn start(name: &str, requests: usize, calls: usize) -> Self {
        let identity_dir = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
        let data_path =
            std::env::temp_dir().join(format!("goal-latency-{name}-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(&data_path)
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let mut hook =
            DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
                .with_goal_tool_authority(true, true);
        assert!(matches!(
            hook.on_completion_call(&user_text_message("start"), &[])
                .await,
            HookAction::Continue
        ));
        let session = hook.session_id().await.unwrap();
        bind_managed_hook_request(
            &mut hook,
            "goal-request",
            &identity,
            Utc::now() + chrono::Duration::minutes(30),
        )
        .await;
        let access = hook.actor_access().unwrap();
        let create = r#"{"objective":"ship the long session","token_budget":100000000}"#;
        accept_hook_tool_call(
            &hook,
            "goal-create",
            crate::goal::CREATE_GOAL_TOOL_NAME,
            create,
            None,
        )
        .await;
        assert!(matches!(
            hook.on_tool_call(
                crate::goal::CREATE_GOAL_TOOL_NAME,
                None,
                "goal-create",
                create
            )
            .await,
            ToolCallHookAction::Skip { .. }
        ));
        let caller_doc_id = access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{request_id: {{_eq: "goal-request"}}, session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                escape_graphql_string(&session)
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let session_doc_id = access
            .execute(&format!(
                r#"{{AgentSession(filter: {{session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                escape_graphql_string(&session)
            ))
            .await
            .unwrap()["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let agent_did = hook
            .actor
            .as_ref()
            .expect("bound hook actor")
            .as_str()
            .to_owned();
        let agent_signer = hook
            .agent_signer
            .clone()
            .expect("bound hook managed Agent signer");
        let mut fixture = Self {
            node,
            identity,
            hook,
            session,
            caller_doc_id,
            session_doc_id,
            agent_did,
            agent_signer,
            access,
            data_path,
            call: 0,
            _identity_dir: identity_dir,
        };
        for index in 0..requests {
            fixture.add_session_request(index).await;
        }
        let visible = fixture
            .access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                escape_graphql_string(&fixture.session)
            ))
            .await
            .unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .len();
        assert!(
            visible > requests,
            "the working Agent must read the session requests this workload adds: {visible}"
        );
        for index in 0..calls {
            fixture.add_inference_call(index, 1_000, 200).await;
        }
        fixture
    }

    /// The session's own working Agent authors and owns its requests, as the
    /// admission fixture does, so the Agent's own reads see them.
    async fn add_session_request(&mut self, index: usize) -> String {
        let request_id = format!("session-request-{index}");
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &request_id,
            self.identity.did(),
            &self.agent_did,
            &self.agent_did,
            &self.session,
            "long session prompt body. ".repeat(80),
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(&self.agent_did),
        );
        request.session_doc_id = Some(self.session_doc_id.clone());
        crate::sign_agent_request_create(&self.agent_signer, &mut request)
            .await
            .unwrap();
        let response = self
            .access
            .write(
                "test.goal_latency_request",
                &request.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let doc = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        self.access
            .write(
                "test.goal_latency_request_terminal",
                &format!(
                    r#"mutation {{update_AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
                    escape_graphql_string(&doc)
                ),
            )
            .await
            .unwrap();
        doc
    }

    async fn add_inference_call(&self, index: usize, prompt: i64, completion: i64) {
        let input = serde_json::json!({
            "call_id": format!("goal-latency-call-{index}"),
            "runtime_instance_id": "goal-latency-test",
            "request_id": "goal-request",
            "request_doc_id": self.caller_doc_id,
            "session_id": self.session,
            "call_seq": index as i64 + 1,
            "backend_id": "goal-latency-backend",
            "agent_did": self.agent_did,
            "node_did": self.identity.did(),
            "call_kind": "inference",
            "attempt": 1,
            "call_state": "completed",
            "queued_at": "2026-10-21T00:00:00Z",
            "started_at": "2026-10-21T00:00:00Z",
            "ended_at": "2026-10-21T00:00:01Z",
            "priority": 0,
            "queue_depth_at_enqueue": 0,
            "controller_generation": 1,
            "backend_config_fingerprint": "goal-latency-test",
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "cached_input_tokens": 0,
        });
        let session_doc = self.session_doc_id.clone();
        self.access
            .transact_write("test.goal_latency_inference_call", |txn| {
                let input = input.clone();
                let session_doc = session_doc.clone();
                Box::pin(async move {
                    let created = txn
                        .execute_with_variables(
                            "mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input: $input) {_docID}}",
                            &serde_json::json!({"input": input}),
                        )
                        .await?;
                    let doc = crate::store::graphql::created_doc_id(&created, "InferenceCall")?;
                    txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", &session_doc)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    async fn set_status(&self, status: &str) {
        self.access
            .write(
                "test.goal_latency_status",
                &format!(
                    r#"mutation {{update_Goal(filter: {{session_id: {{_eq: "{}"}}}}, input: {{status: "{status}", active_started_at: null}}) {{_docID}}}}"#,
                    escape_graphql_string(&self.session)
                ),
            )
            .await
            .unwrap();
    }

    async fn get_goal(&mut self) -> (Value, StatementCounts) {
        self.call += 1;
        let id = format!("goal-get-{}", self.call);
        accept_hook_tool_call(&self.hook, &id, crate::goal::GET_GOAL_TOOL_NAME, "{}", None).await;
        let (action, counts) = count_statements(self.hook.on_tool_call(
            crate::goal::GET_GOAL_TOOL_NAME,
            None,
            &id,
            "{}",
        ))
        .await;
        let reason = match action {
            ToolCallHookAction::Skip { reason } | ToolCallHookAction::Terminate { reason } => {
                reason
            }
            action => panic!("get_goal escaped its accepted hook: {action:?}"),
        };
        (serde_json::from_str(&reason).unwrap(), counts)
    }

    async fn shutdown(self) {
        self.node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(&self.data_path);
    }
}

#[tokio::test]
async fn get_goal_observes_its_assignment_once() {
    let mut fixture = GoalLatencySession::start("observes-once", 4, 3).await;
    let (reply, counts) = fixture.get_goal().await;
    assert_eq!(
        counts.transactions("goal.tool_context"),
        1,
        "get_goal re-derived its immutable attribution: {:?}",
        counts
            .statements()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        session_request_scans(&counts),
        1,
        "get_goal scanned the session's request bodies more than once"
    );
    assert_eq!(reply["objective"], "ship the long session");
    assert_eq!(reply["status"], "active");
    assert_eq!(reply["read_only"], false);
    assert_eq!(reply["token_budget"], 100_000_000);
    assert_eq!(
        reply["tokens_used"].as_i64().unwrap(),
        3 * 1_200,
        "the reply must report the charged session total"
    );
    assert!(reply["active_time_seconds"].as_i64().unwrap() >= 0);
    assert_eq!(
        goal_tokens_used(&fixture.access, &fixture.session).await,
        3 * 1_200,
        "the canonical Goal must carry the refreshed total"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn get_goal_writes_no_goal_revision_when_usage_is_unchanged() {
    for status in ["active", "paused"] {
        let mut fixture = GoalLatencySession::start(status, 2, 2).await;
        if status == "paused" {
            fixture.set_status(status).await;
        }
        let (first, _) = fixture.get_goal().await;
        assert_eq!(first["status"], status);
        let settled = goal_commits(&fixture.access).await;
        let (second, counts) = fixture.get_goal().await;
        assert_eq!(second["status"], status, "{status}");
        assert_eq!(
            second["tokens_used"], first["tokens_used"],
            "{status}: unchanged usage reported differently"
        );
        assert_eq!(
            goal_commits(&fixture.access).await,
            settled,
            "{status}: a read of unchanged usage wrote a Goal revision"
        );
        assert_eq!(
            counts.transactions("goal.update_controller_fields"),
            0,
            "{status}: a read of unchanged usage opened a write transaction"
        );
        fixture.shutdown().await;
    }
}

/// Usage is summed over the session, not over a goal's continuations, so a
/// paused goal's charged total still moves while the session is used. The
/// elision is a value comparison and must never become a status gate.
#[tokio::test]
async fn get_goal_refreshes_tokens_when_inference_lands_while_paused() {
    let mut fixture = GoalLatencySession::start("paused-inference", 2, 1).await;
    fixture.set_status("paused").await;
    let (first, _) = fixture.get_goal().await;
    assert_eq!(first["status"], "paused");
    assert_eq!(first["tokens_used"].as_i64().unwrap(), 1_200);
    let settled = goal_commits(&fixture.access).await;
    fixture.add_inference_call(99, 5_000, 500).await;
    let (second, counts) = fixture.get_goal().await;
    assert_eq!(second["status"], "paused");
    assert_eq!(
        second["tokens_used"].as_i64().unwrap(),
        1_200 + 5_500,
        "a paused goal must still observe the session's new charges"
    );
    assert!(
        goal_commits(&fixture.access).await > settled,
        "a changed total must be written as a Goal revision"
    );
    assert_eq!(counts.transactions("goal.update_controller_fields"), 1);
    assert_eq!(
        goal_tokens_used(&fixture.access, &fixture.session).await,
        1_200 + 5_500
    );
    fixture.shutdown().await;
}

/// The database work one `get_goal` performs does not grow with the session,
/// including the Goal revisions earlier calls left behind.
#[tokio::test]
async fn get_goal_statements_stay_constant_as_the_session_grows() {
    let mut measured = Vec::new();
    for (name, size) in [("short", 1_usize), ("long", 25_usize)] {
        let mut fixture = GoalLatencySession::start(name, size, size).await;
        for _ in 0..size {
            fixture.get_goal().await;
        }
        let (_, counts) = fixture.get_goal().await;
        measured.push((
            size,
            counts.transactions("goal.tool_context"),
            session_request_scans(&counts),
            counts.for_operation("goal.tool_context"),
        ));
        fixture.shutdown().await;
    }
    assert_eq!(
        measured[0].1, measured[1].1,
        "transactions grew with session length: {measured:?}"
    );
    assert_eq!(
        measured[0].2, measured[1].2,
        "session scans grew with session length: {measured:?}"
    );
    assert_eq!(
        measured[0].3, measured[1].3,
        "attribution statements grew with session length: {measured:?}"
    );
}

/// Reports the measured shape of one `get_goal`; elapsed time is an
/// observation of this workload, never an assertion.
#[tokio::test]
#[ignore = "profile: reports measurements rather than asserting them"]
async fn get_goal_long_session_profile() {
    let subscriber = tracing_subscriber::fmt().with_test_writer().finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    for size in [1_usize, 25, 100] {
        for status in ["active", "paused"] {
            let mut fixture =
                GoalLatencySession::start(&format!("profile-{size}-{status}"), size, size).await;
            if status == "paused" {
                fixture.set_status(status).await;
            }
            for _ in 0..size {
                fixture.get_goal().await;
            }
            let commits_before = goal_commits(&fixture.access).await;
            let started = std::time::Instant::now();
            let (_, counts) = fixture.get_goal().await;
            let elapsed_ms = started.elapsed().as_millis();
            tracing::info!(
                size,
                status,
                prior_get_goal_calls = size,
                statements_total = counts.total(),
                mutations = counts.mutations(),
                tool_context_statements = counts.for_operation("goal.tool_context"),
                tool_context_transactions = counts.transactions("goal.tool_context"),
                usage_statements = counts.for_operation("goal.session_usage"),
                controller_statements = counts.for_operation("goal.update_controller_fields"),
                session_request_scans = session_request_scans(&counts),
                goal_commits_before = commits_before,
                goal_commits_after = goal_commits(&fixture.access).await,
                elapsed_ms,
                "get_goal long session profile"
            );
            fixture.shutdown().await;
        }
    }
}
