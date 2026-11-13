use std::time::Duration;

use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_session::request::interrupt::interrupt_request_by_doc_id_with_access;
use gents_session::request::lifecycle::RequestLifecycle;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::support::agent_requests::{submit_user_request, RequestActors};
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_ceiling, live_target, wait_for_request_terminal,
    wait_for_rows,
};
use crate::support::snapshots::{
    fetch_message_snapshots_for_session_as, fetch_tool_call_snapshots_for_session, SnapshotReader,
};
use crate::support::{test_db, TestDb};

const WAIT: Duration = Duration::from_secs(180);

struct Recovery {
    db: TestDb,
    actors: RequestActors,
    runtime: Option<BootedAgent>,
    root: tempfile::TempDir,
}

impl Recovery {
    async fn new(name: &str) -> Self {
        let target = live_target().await;
        let db = test_db(name).await;
        let actors = bind_target(&db, &target).await;
        let root = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .unwrap()
            .unwrap();
        configure_agent_tools_as(
            &actors.fixture.access,
            actors.node_did(),
            actors.agent_did(),
            Some("Call the requested tool with the supplied arguments. After it returns, acknowledge briefly without calling another tool. Background completion messages need only an acknowledgement.".into()),
            Tools {
                tools_id: "live-recovery-tools".into(),
                node_did: actors.node_did().into(),
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
        ).await;
        let runtime = boot_live_agent_with_ceiling(
            &db,
            db.node_identity.clone(),
            ToolCeiling::readonly_at(root.path()),
        )
        .await
        .unwrap();
        Self {
            db,
            actors,
            runtime: Some(runtime),
            root,
        }
    }

    async fn submit(&self, id: &str, tool: &str, args: Value) -> String {
        submit_user_request(
            self.actors.node_did(),
            &self.actors.fixture,
            id,
            id,
            &format!("Call {tool} with these arguments: {args}. Then acknowledge briefly without additional tool calls."),
            Default::default(),
        ).await
    }

    async fn call(&self, request: &str, state: &str) -> Value {
        let query = format!(
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "bash"}}, lifecycle_state: {{_eq: "{}"}}}}) {{_docID request_doc_id node_did requester_did session_id lifecycle_state cancel_cause completion_notification_delivered_at}} }}"#,
            escape_graphql_string(request),
            escape_graphql_string(state)
        );
        wait_for_rows(
            &self.actors.fixture.access,
            "AgentToolCall",
            &query,
            WAIT,
            |rows| {
                rows["AgentToolCall"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
            },
        )
        .await["AgentToolCall"][0]
            .clone()
    }

    async fn held(&self, id: &str) -> (Value, tokio::fs::File) {
        let fifo = self.root.path().join(id);
        let created = tokio::time::timeout(
            WAIT,
            tokio::process::Command::new("mkfifo").arg(&fifo).status(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(created.success(), "create held recovery process FIFO");
        let gate_path = fifo.clone();
        let gate = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(gate_path)
        })
        .await
        .unwrap()
        .unwrap();
        let gate = tokio::fs::File::from_std(gate);
        self.submit(id, "bash", json!({"command":"sh", "args":["-c", "exec 3< \"$1\"; printf READY; IFS= read -r release <&3; printf FINISHED", "gents-recovery", fifo.display().to_string()]})).await;
        let call = self.call(id, "running").await;
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut changes = self
            .actors
            .fixture
            .access
            .subscribe_changes("AgentOutputSegment")
            .await;
        loop {
            let output = gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &self.actors.fixture.access,
                call["_docID"].as_str().unwrap(),
                call["request_doc_id"].as_str().unwrap(),
                id,
                self.actors.node_did(),
                call["requester_did"].as_str(),
            ).await.unwrap().into_text();
            if output.contains("READY") {
                return (call, gate);
            }
            tokio::time::timeout_at(deadline, changes.recv())
                .await
                .expect("held process start deadline");
        }
    }

    async fn recover_tools(&self) {
        ToolCallLifecycle::recover_all(
            &self.db.node,
            self.actors.node_did(),
            std::slice::from_ref(self.actors.agent()),
        )
        .await
        .unwrap();
    }

    async fn crash(&mut self) {
        self.runtime.take().unwrap().crash().await;
    }

    async fn shutdown(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
    }
}

async fn patch(access: &ConfigAccess, collection: &str, doc: &str, value: Value) {
    let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
    access
        .write(
            "test.live_recovery_fault",
            &format!(
                r#"mutation {{ update_{collection}(docID: "{}", input: {input}) {{ _docID }} }}"#,
                escape_graphql_string(doc)
            ),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_all_times_out_expired_running_tool_calls() {
    let mut recovery = Recovery::new("live-recover-expired-tool").await;
    let (call, mut gate) = recovery.held("expired-tool").await;
    recovery.crash().await;
    patch(
        &recovery.actors.agent_access,
        "AgentToolCall",
        call["_docID"].as_str().unwrap(),
        json!({"deadline_at":"2020-01-01T00:00:00Z"}),
    )
    .await;
    recovery.recover_tools().await;
    let timed_out = recovery.call("expired-tool", "timedOut").await;
    assert_eq!(timed_out["_docID"], call["_docID"]);
    assert_eq!(timed_out["cancel_cause"], "deadline");
    let snapshots =
        fetch_tool_call_snapshots_for_session(&recovery.actors.fixture.access, "expired-tool")
            .await;
    let result = snapshots
        .iter()
        .find(|row| row.doc_id == call["_docID"].as_str().unwrap())
        .unwrap()
        .try_load_result_as(&recovery.actors.fixture.access)
        .await
        .unwrap();
    assert!(result.contains("deadline exceeded"), "{result}");
    gate.write_all(b"release\n").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_all_repairs_terminal_background_tool_notification_once() {
    let mut recovery = Recovery::new("live-recover-notification").await;
    recovery.submit("notification", "spawn_process",
        json!({"tool_name":"bash", "args":{"command":"printf", "args":["durable recovery result"]}})).await;
    let call = recovery.call("notification", "completed").await;
    assert_eq!(
        wait_for_request_terminal(&recovery.actors.fixture.access, "notification", WAIT).await,
        "completed"
    );
    recovery.shutdown().await;
    patch(
        &recovery.actors.agent_access,
        "AgentToolCall",
        call["_docID"].as_str().unwrap(),
        json!({"status":"completionPending", "completion_notification_delivered_at":null}),
    )
    .await;
    recovery.recover_tools().await;
    let repaired = recovery.call("notification", "completed").await;
    assert!(repaired["completion_notification_delivered_at"]
        .as_str()
        .is_some());
    let delivered =
        fetch_message_snapshots_for_session_as(&recovery.actors.fixture.access, "notification")
            .await;
    let notifications: Vec<_> = delivered
        .iter()
        .filter(|message| {
            message
                .message_key
                .starts_with("background-completion-notification:")
                && message.content.contains("durable recovery result")
        })
        .map(|message| (&message.message_key, &message.content))
        .collect();
    assert!(
        !notifications.is_empty(),
        "recovery must deliver the completed native tool output"
    );
    recovery.recover_tools().await;
    let repeated =
        fetch_message_snapshots_for_session_as(&recovery.actors.fixture.access, "notification")
            .await;
    let repeated_notifications: Vec<_> = repeated
        .iter()
        .filter(|message| {
            message
                .message_key
                .starts_with("background-completion-notification:")
                && message.content.contains("durable recovery result")
        })
        .map(|message| (&message.message_key, &message.content))
        .collect();
    assert_eq!(
        repeated_notifications, notifications,
        "repeating recovery must not redeliver an existing notification"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_all_cancels_running_tool_call_for_interrupted_parent_only() {
    let mut recovery = Recovery::new("live-recover-interrupted-parent").await;
    let (interrupted, mut interrupted_gate) = recovery.held("interrupted-tool").await;
    let (unrelated, mut unrelated_gate) = recovery.held("unrelated-tool").await;
    recovery.crash().await;
    let request_doc = interrupted["request_doc_id"].as_str().unwrap();
    interrupt_request_by_doc_id_with_access(
        &recovery.actors.agent_access,
        request_doc,
        recovery.actors.node_did(),
        interrupted["requester_did"].as_str(),
    )
    .await
    .unwrap();
    patch(
        &recovery.actors.agent_access,
        "AgentRequest",
        request_doc,
        json!({"execution_lease_expires_at":"2020-01-01T00:00:00Z"}),
    )
    .await;
    patch(
        &recovery.actors.agent_access,
        "AgentRequest",
        unrelated["request_doc_id"].as_str().unwrap(),
        json!({"execution_lease_expires_at":"2099-01-01T00:00:00Z"}),
    )
    .await;
    RequestLifecycle::recover_all_with_identities(
        &recovery.db.node,
        recovery.actors.node_did(),
        std::slice::from_ref(recovery.actors.agent()),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_for_request_terminal(&recovery.actors.fixture.access, "interrupted-tool", WAIT).await,
        "interrupted"
    );
    recovery.recover_tools().await;
    let cancelled = recovery.call("interrupted-tool", "cancelled").await;
    assert_eq!(cancelled["_docID"], interrupted["_docID"]);
    assert_eq!(cancelled["cancel_cause"], "interrupted");
    let unaffected = recovery.call("unrelated-tool", "running").await;
    assert_eq!(unaffected["_docID"], unrelated["_docID"]);
    interrupted_gate.write_all(b"release\n").await.unwrap();
    unrelated_gate.write_all(b"release\n").await.unwrap();
}
