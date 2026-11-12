use std::time::Duration;

use gents::{DocumentRuntimeOptions, Gents};
use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_session::request::interrupt::interrupt_request_by_doc_id_with_access;
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::hook::BackgroundExecutionRegistry;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

use crate::support::fixtures::configure_agent_tools;
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{
    live_target, wait_for_request_terminal, wait_for_rows, LiveAgentSpec, LiveUser,
};
use crate::support::snapshots::SnapshotReader;
use crate::support::{test_db, TestDb};

const WAIT: Duration = Duration::from_secs(240);

struct Processes {
    db: TestDb,
    live: LiveUser,
    runtime: Option<BootedAgent>,
    registry: BackgroundExecutionRegistry,
    root: tempfile::TempDir,
    primary: String,
    secondary: String,
    gates: tokio::sync::Mutex<Vec<std::fs::File>>,
}

impl Processes {
    async fn new(name: &str) -> Self {
        Self::with_deadline(name, None).await
    }

    async fn with_deadline(name: &str, deadline: Option<i64>) -> Self {
        let target = live_target().await;
        let db = test_db(name).await;
        let live = LiveUser::install(&db, &target).await;
        let root = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .unwrap()
            .unwrap();
        let mut agents = Vec::new();
        for (name, default) in [("primary", true), ("secondary", false)] {
            let agent = live
                .agent(
                    &target,
                    LiveAgentSpec {
                        display_name: name,
                        inference_profile_id: &format!("process-{name}-profile"),
                        system_prompt: "Call the tool named in the latest request with its exact arguments. After it returns, acknowledge briefly. Do not call additional tools. A background completion only needs acknowledgement.",
                        description: None,
                        default_for_node: default,
                    },
                )
                .await;
            if let Some(seconds) = deadline {
                let execution_id = format!("process-{name}-execution");
                let mut profile = target.profile(&live.node_did);
                profile.profile_id = format!("process-{name}-profile");
                profile.execution_id = Some(execution_id.clone());
                profile.sampling_id = Some(format!("{agent}:live-sampling"));
                live.apply(
                    "test.process_deadline",
                    vec![
                        (
                            Collection::InferenceExecution,
                            serde_json::to_value(gents_documents::InferenceExecution {
                                node_did: live.node_did.clone(),
                                execution_id,
                                stream_liveness_timeout_secs: Some(5),
                                deadline_duration_secs: Some(seconds),
                                ..Default::default()
                            })
                            .unwrap(),
                        ),
                        (
                            Collection::InferenceProfile,
                            serde_json::to_value(profile).unwrap(),
                        ),
                    ],
                )
                .await;
            }
            configure_agent_tools(
                &db,
                &live.node_did,
                &agent,
                None,
                Tools {
                    tools_id: format!("process-{name}-tools"),
                    node_did: live.node_did.clone(),
                    host: Some(HostTools {
                        root: Some(root.path().display().to_string()),
                        bash: Some(BashTools {
                            mode: BashMode::ReadOnly,
                            read_only_commands: Some(vec!["sh".into(), "printf".into()]),
                            background_enabled: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Vec::new(),
            )
            .await;
            agents.push(agent);
        }
        let runtime = Gents::from_default_agent_documents(
            db.node.clone(),
            db.node_identity.clone(),
            DocumentRuntimeOptions {
                backend_admission: Some(db.home.backend_admission()),
                tool_ceiling: ToolCeiling::readonly_at(root.path()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let registry = runtime.background_execution_registry();
        let node_did = runtime.node_did().to_owned();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(runtime.run(receiver));
        wait_for_runtime_ready(&db.node, &node_did).await;
        Self {
            db,
            live,
            runtime: Some(BootedAgent::new(shutdown, task, node_did)),
            registry,
            root,
            primary: agents.remove(0),
            secondary: agents.remove(0),
            gates: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn request(
        &self,
        agent: &str,
        id: &str,
        session: &str,
        tool: &str,
        args: Value,
    ) -> String {
        self.live.request(agent, id, session, &format!("Call {tool} with exactly these arguments: {args}. After it returns, acknowledge briefly and call no other tool.")).await
    }

    async fn call(&self, request: &str, tool: &str) -> Value {
        let query = format!(
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "{}"}}}}) {{_docID tool_call_id request_doc_id session_id node_did requester_did lifecycle_state cancel_cause completion_notification_delivered_at}} }}"#,
            escape_graphql_string(request),
            escape_graphql_string(tool)
        );
        wait_for_rows(&self.live, "AgentToolCall", &query, WAIT, |data| {
            data["AgentToolCall"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        })
        .await["AgentToolCall"][0]
            .clone()
    }

    async fn state(&self, call: &Value, expected: &str) -> Value {
        let query = format!(
            r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID lifecycle_state cancel_cause completion_notification_delivered_at}} }}"#,
            escape_graphql_string(call["_docID"].as_str().unwrap())
        );
        wait_for_rows(&self.live, "AgentToolCall", &query, WAIT, |data| {
            data["AgentToolCall"][0]["lifecycle_state"] == expected
        })
        .await["AgentToolCall"][0]
            .clone()
    }

    async fn output(&self, call: &Value, marker: &str) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut changes = self.live.subscribe_changes("AgentOutputSegment").await;
        loop {
            let output = gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &self.live.access,
                call["_docID"].as_str().unwrap(),
                call["request_doc_id"].as_str().unwrap(),
                call["session_id"].as_str().unwrap(),
                &self.live.node_did,
                call["requester_did"].as_str(),
            ).await.unwrap().into_text();
            if output.contains(marker) {
                return output;
            }
            tokio::time::timeout_at(deadline, changes.recv())
                .await
                .expect("process output deadline");
        }
    }

    async fn gated(
        &self,
        agent: &str,
        id: &str,
        session: &str,
        background: bool,
    ) -> (String, Value) {
        let fifo = self.root.path().join(id);
        assert!(tokio::time::timeout(
            WAIT,
            tokio::process::Command::new("mkfifo").arg(&fifo).status()
        )
        .await
        .unwrap()
        .unwrap()
        .success());
        let retained_fifo = fifo.clone();
        let retained = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(retained_fifo)
        })
        .await
        .unwrap()
        .unwrap();
        self.gates.lock().await.push(retained);
        let args = json!({"command":"sh", "args":["-c", "exec 3< \"$1\"; printf READY; IFS= read -r release <&3; printf FINISHED", "gents-live-process", fifo.display().to_string()]});
        let request = if background {
            self.request(
                agent,
                id,
                session,
                "spawn_process",
                json!({"tool_name":"bash", "args":args}),
            )
            .await
        } else {
            self.request(agent, id, session, "bash", args).await
        };
        let call = self.call(id, "bash").await;
        self.output(&call, "READY").await;
        if background {
            assert_eq!(
                wait_for_request_terminal(&self.live, id, WAIT).await,
                "completed"
            );
        }
        (request, call)
    }

    async fn release(&self, id: &str) {
        tokio::fs::write(self.root.path().join(id), "release\n")
            .await
            .unwrap();
    }

    async fn interrupt(&self, request: &str) {
        interrupt_request_by_doc_id_with_access(
            &self.live.access,
            request,
            &self.live.node_did,
            Some(self.live.user_did()),
        )
        .await
        .unwrap();
    }

    async fn result(&self, call: &Value) -> Value {
        let result = gents_session::tool_call::query::result::load_tool_call_result(
            &self.live.access,
            call["_docID"].as_str().unwrap(),
            &self.live.node_did,
            call["session_id"].as_str().unwrap(),
            Some(self.live.user_did()),
        )
        .await
        .unwrap();
        let text = gents_session::tool_call::query::result::render_tool_result(&result).unwrap();
        serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("expected structured process result: {text}"))
    }

    async fn identity(&self, agent: &str) -> std::sync::Arc<dyn NodeIdentity> {
        let config = gents_documents::load_agent(&self.db.node, &self.live.node_did, agent)
            .await
            .unwrap()
            .unwrap();
        gents_store::identity::resolve_agent_identity(
            self.db.node_identity.as_ref(),
            &config.identity_binding(),
        )
        .unwrap()
    }

    async fn access(&self, agent: &str) -> gents_store::config_client::ConfigAccess {
        let identity = self.identity(agent).await;
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            identity.did().parse().unwrap(),
            identity.as_ref(),
        )
        .unwrap()
    }

    async fn patch(&self, collection: &str, doc: &str, fields: Value) {
        let fields = gents_protocol::graphql::graphql_input_literal(&fields).unwrap();
        self.access(&self.primary).await.write("test.process_fault", &format!(
            "mutation {{update_{collection}(filter: {{_docID: {{_eq: \"{}\"}}}}, input: {fields}) {{_docID}}}}", escape_graphql_string(doc)
        )).await.unwrap();
    }

    async fn control(&self, id: &str, session: &str, tool: &str, args: Value) -> Value {
        self.request(&self.primary, id, session, tool, args).await;
        assert_eq!(
            wait_for_request_terminal(&self.live, id, WAIT).await,
            "completed"
        );
        let call = self.call(id, tool).await;
        self.result(&call).await
    }

    async fn sweep(&self, registered: bool) {
        let identities = vec![
            self.identity(&self.primary).await,
            self.identity(&self.secondary).await,
        ];
        let empty = BackgroundExecutionRegistry::default();
        gents_toolcalls::call::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
            &self.db.node,
            &self.live.node_did,
            &identities,
            if registered { &self.registry } else { &empty },
            &gents_automation::goal::session_message::SessionGoals,
        )
        .await
        .unwrap();
    }

    async fn stop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupting_one_request_does_not_affect_another() {
    let mut processes = Processes::new("live-interruption-isolation").await;
    let (target, target_call) = processes
        .gated(&processes.primary, "target", "target-session", false)
        .await;
    let (_, survivor_call) = processes
        .gated(&processes.secondary, "survivor", "survivor-session", false)
        .await;
    processes.interrupt(&target).await;
    assert_eq!(
        wait_for_request_terminal(&processes.live, "target", WAIT).await,
        "interrupted"
    );
    processes.state(&target_call, "cancelled").await;
    processes.state(&survivor_call, "running").await;
    processes.release("survivor").await;
    assert_eq!(
        wait_for_request_terminal(&processes.live, "survivor", WAIT).await,
        "completed"
    );
    processes.state(&survivor_call, "completed").await;
    processes.output(&survivor_call, "FINISHED").await;
    processes.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_process_bounded_wait_returns_still_running_without_cancelling() {
    let mut p = Processes::new("live-bounded-wait").await;
    let (_, job) = p.gated(&p.primary, "job", "session", true).await;
    let result = p
        .control(
            "wait",
            "session",
            "wait_process",
            json!({"tool_call_id":job["tool_call_id"], "timeout_secs":1}),
        )
        .await;
    assert_eq!(result["status"], "running");
    assert_eq!(result["error"]["reason"], "wait_timeout");
    let running = p.state(&job, "running").await;
    assert!(running["cancel_cause"].is_null());
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_envelope_bounds_oversized_background_tool_result() {
    let mut p = Processes::new("live-bounded-process-output").await;
    p.request(
        &p.primary,
        "produce",
        "session",
        "spawn_process",
        json!({"tool_name":"bash", "args":{"command":"sh", "args":["-c", "printf '%100000s' x"]}}),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&p.live, "produce", WAIT).await,
        "completed"
    );
    let job = p.call("produce", "bash").await;
    p.state(&job, "completed").await;
    let result = p
        .control(
            "wait",
            "session",
            "wait_process",
            json!({"tool_call_id":job["tool_call_id"], "timeout_secs":120}),
        )
        .await;
    assert_eq!(result["status"], "completed");
    let bounded = result["result"].as_str().unwrap();
    assert!(!bounded.is_empty());
    assert!(bounded.len() < 100_000);
    let persisted = p.output(&job, "x").await;
    assert!(persisted.len() >= 100_000);
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_tool_rejects_not_allowlisted_target() {
    let mut p = Processes::new("live-unavailable-process-tool").await;
    let result = p
        .control(
            "denied",
            "session",
            "spawn_process",
            json!({"tool_name":"other_tool", "args":{}}),
        )
        .await;
    assert_eq!(result["failure_class"], "tool_not_allowed");
    assert_eq!(result["requested_tool_name"], "other_tool");
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_process_survives_creator_and_waiter_deadlines() {
    let mut p = Processes::with_deadline("live-process-deadlines", Some(30)).await;
    let (_, job) = p.gated(&p.primary, "job", "session", true).await;
    p.request(
        &p.primary,
        "wait",
        "session",
        "wait_process",
        json!({"tool_call_id":job["tool_call_id"], "timeout_secs":120}),
    )
    .await;
    let waiter = p.call("wait", "wait_process").await;
    p.state(&waiter, "running").await;
    assert_eq!(
        wait_for_request_terminal(&p.live, "wait", WAIT).await,
        "failed"
    );
    let stopped = p.state(&waiter, "timedOut").await;
    assert_eq!(stopped["cancel_cause"], "deadline");
    let running = p.state(&job, "running").await;
    assert!(running["cancel_cause"].is_null());
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_tool_caller_interrupt_returns_without_cancelling_background_row() {
    let mut p = Processes::new("live-process-interrupt").await;
    let (_, job) = p.gated(&p.primary, "job", "session", true).await;
    let request = p
        .request(
            &p.primary,
            "wait",
            "session",
            "wait_process",
            json!({"tool_call_id":job["tool_call_id"], "timeout_secs":120}),
        )
        .await;
    let waiter = p.call("wait", "wait_process").await;
    p.state(&waiter, "running").await;
    p.interrupt(&request).await;
    assert_eq!(
        wait_for_request_terminal(&p.live, "wait", WAIT).await,
        "interrupted"
    );
    let running = p.state(&job, "running").await;
    assert!(running["cancel_cause"].is_null());
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}

async fn registered_process_survives_recovery(interrupted: bool) {
    let mut p = Processes::new("live-registered-process-recovery").await;
    let (request, job) = p.gated(&p.primary, "job", "session", true).await;
    if interrupted {
        p.patch(
            "AgentRequest",
            &request,
            json!({"lifecycle_state":"interrupted"}),
        )
        .await;
    }
    p.sweep(true).await;
    let running = p.state(&job, "running").await;
    assert!(running["cancel_cause"].is_null());
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn periodic_recovery_does_not_terminalize_registered_background_worker() {
    registered_process_survives_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn periodic_recovery_preserves_registered_worker_after_parent_interrupt() {
    registered_process_survives_recovery(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn periodic_recovery_without_process_record_settles_orphan_lost() {
    let mut p = Processes::new("live-orphan-process-recovery").await;
    let (_, job) = p.gated(&p.primary, "job", "session", true).await;
    p.patch(
        "AgentToolCall",
        job["_docID"].as_str().unwrap(),
        json!({"deadline_at":"2020-01-01T00:00:00Z"}),
    )
    .await;
    p.sweep(false).await;
    let failed = p.state(&job, "failed").await;
    assert!(failed["cancel_cause"].is_null());
    p.release("job").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_running_row_does_not_hide_valid_orphan_recovery() {
    let mut p = Processes::new("live-malformed-orphan-recovery").await;
    let (request, job) = p.gated(&p.primary, "job", "session", true).await;
    p.patch(
        "AgentRequest",
        &request,
        json!({"lifecycle_state":"interrupted"}),
    )
    .await;
    p.access(&p.primary).await.write("test.malformed_process", &format!(
        r#"mutation {{create_AgentToolCall(input: {{tool_call_key: "malformed-recovery", node_did: "{}", lifecycle_state: "running", await_mode: "background"}}) {{_docID}}}}"#,
        escape_graphql_string(&p.live.node_did)
    )).await.unwrap();
    p.sweep(false).await;
    p.state(&job, "failed").await;
    let rows = p.live.read_data(r#"{AgentToolCall(filter: {tool_call_key: {_eq: "malformed-recovery"}}) {lifecycle_state}}"#).await;
    assert_eq!(rows["AgentToolCall"][0]["lifecycle_state"], "running");
    p.release("job").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_processes_skips_malformed_legacy_rows_without_hiding_valid_jobs() {
    let mut p = Processes::new("live-malformed-process-list").await;
    let (_, job) = p.gated(&p.primary, "job", "session", true).await;
    p.access(&p.primary).await.write("test.malformed_process", &format!(
        r#"mutation {{create_AgentToolCall(input: {{tool_call_key: "malformed-list", session_id: "session", node_did: "{}", lifecycle_state: "running", await_mode: "background"}}) {{_docID}}}}"#,
        escape_graphql_string(&p.live.node_did)
    )).await.unwrap();
    let result = p
        .control("list", "session", "list_processes", json!({}))
        .await;
    let entries = result["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|row| row["tool_call_id"] == job["tool_call_id"]));
    assert!(entries.iter().all(|row| row["tool_call_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty())));
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_signed_principal_runtime_is_denied_foreign_process_handle() {
    let mut p = Processes::new("live-foreign-process-handle").await;
    let (_, job) = p.gated(&p.primary, "job", "owner-session", true).await;
    let foreign = tokio::task::spawn_blocking(|| {
        crate::support::fixtures::test_identity("foreign-process-user")
    })
    .await
    .unwrap();
    let did = identity::Did::new(foreign.did()).unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        p.db.node.clone(),
        identity::Did::new(&p.live.node_did).unwrap(),
    );
    for permission in [
        gents_store::config_client::NodePermission::CollectionGet,
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
        gents_store::config_client::NodePermission::DacRelationAdd,
    ] {
        admin.grant_node_permission(&did, permission).await.unwrap();
    }
    let (agent_doc, _) =
        gents_documents::load_agent_record(&p.db.node, &p.live.node_did, &p.primary)
            .await
            .unwrap()
            .unwrap();
    for relationship in ["reader", "session_writer"] {
        p.live
            .access
            .add_actor_relationship("Agent", &agent_doc, relationship, &did)
            .await
            .unwrap();
    }
    let foreign_access = p.db.client_access(&foreign);
    crate::support::agent_requests::submit_user_request_to(
        &foreign_access, &foreign, &p.live.node_did, &p.primary,
        "foreign-session", "foreign-read",
        &format!("Call read_process with exactly {{\"tool_call_id\":{}}}. Do not list or search for another process. After the error, acknowledge briefly.", job["tool_call_id"]),
        Default::default(),
    ).await;
    assert_eq!(
        wait_for_request_terminal(&foreign_access, "foreign-read", WAIT).await,
        "completed"
    );
    let rows = foreign_access.read_data(r#"{AgentToolCall(filter: {request_id: {_eq: "foreign-read"}, tool_name: {_eq: "read_process"}}) {_docID}}"#).await;
    let result = gents_session::tool_call::query::result::load_tool_call_result(
        &foreign_access,
        rows["AgentToolCall"][0]["_docID"].as_str().unwrap(),
        &p.live.node_did,
        "foreign-session",
        Some(foreign.did()),
    )
    .await
    .unwrap();
    let result: Value = serde_json::from_str(
        &gents_session::tool_call::query::result::render_tool_result(&result).unwrap(),
    )
    .unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["failure_class"], "tool_not_allowed");
    assert_eq!(result["path"], "/tool_call_id");
    let running = p.state(&job, "running").await;
    assert!(running["cancel_cause"].is_null());
    p.release("job").await;
    p.state(&job, "completed").await;
    p.stop().await;
}
