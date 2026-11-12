use std::time::Duration;

use gents::{DocumentRuntimeOptions, Gents};
use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_session::request::interrupt::interrupt_request_by_doc_id_with_access;
use gents_store::graphql::escape_graphql_string;
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
}

impl Processes {
    async fn new(name: &str) -> Self {
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
        let args = json!({"command":"sh", "args":["-c", "printf READY; IFS= read -r release < \"$1\"; printf FINISHED", "gents-live-process", fifo.display().to_string()]});
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
