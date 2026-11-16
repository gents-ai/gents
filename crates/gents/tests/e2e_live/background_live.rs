use std::time::Duration;

use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

use crate::support::agent_requests::{submit_user_request, RequestActors};
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_ceiling, live_target, wait_for_request_terminal,
    wait_for_rows,
};
use crate::support::snapshots::SnapshotReader;
use crate::support::{test_db, TestDb};

const SESSION: &str = "live-background-session";

struct Fixture {
    _db: TestDb,
    actors: RequestActors,
    runtime: BootedAgent,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let target = live_target().await;
        let db = test_db(name).await;
        let actors = bind_target(&db, &target).await;
        let root = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .unwrap()
            .unwrap();
        tokio::fs::write(root.path().join("waiting.txt"), "")
            .await
            .unwrap();
        crate::support::fixtures::configure_agent_tools_as(&actors.fixture.access, actors.node_did(), actors.agent_did(),
            Some("Follow only the requested tool steps. Stop after they return and acknowledge briefly. A background-completion notification needs only a brief acknowledgement.".into()),
            Tools {
                node_did: actors.node_did().into(),
                tools_id: "live-background-tools".into(),
                host: Some(HostTools {
                    root: Some(root.path().display().to_string()),
                    bash: Some(BashTools {background_enabled: true, mode: BashMode::ReadOnly, read_only_commands: Some(vec!["printf".into(), "tail".into()]), ..Default::default()}),
                    ..Default::default()
                }),
                ..Default::default()
            }, vec![]).await;
        let runtime = boot_live_agent_with_ceiling(
            &db,
            db.node_identity.clone(),
            ToolCeiling::readonly_at(root.path()),
        )
        .await
        .unwrap();
        Self {
            _db: db,
            actors,
            runtime,
            _root: root,
        }
    }

    async fn request(&self, id: &str, prompt: &str) {
        submit_user_request(
            self.actors.node_did(),
            &self.actors.fixture,
            SESSION,
            id,
            prompt,
            Default::default(),
        )
        .await;
        assert_eq!(
            wait_for_request_terminal(&self.actors.fixture.access, id, Duration::from_secs(120))
                .await,
            "completed"
        );
    }

    async fn call(&self, request: &str, name: &str) -> Value {
        let query = format!(
            r#"{{AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "{}"}}}}) {{_docID tool_call_id lifecycle_state await_mode}}}}"#,
            escape_graphql_string(request),
            escape_graphql_string(name)
        );
        let rows = self.actors.fixture.access.read_data(&query).await;
        rows["AgentToolCall"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("requested {name} did not run: {rows}"))
    }

    async fn result(&self, call: &Value) -> Value {
        let message = gents_session::tool_call::query::result::load_tool_call_result(
            &self.actors.fixture.access,
            call["_docID"].as_str().unwrap(),
            self.actors.node_did(),
            SESSION,
            Some(self.actors.fixture.user.did()),
        )
        .await
        .unwrap();
        serde_json::from_str(
            &gents_session::tool_call::query::result::render_tool_result(&message).unwrap(),
        )
        .unwrap()
    }

    async fn spawn(&self, command: &str, args: &[&str]) -> Value {
        self.request("spawn", &format!("Call spawn_process with exactly these arguments: {}. Do not wait for it; acknowledge after the running receipt.", json!({"tool_name":"bash", "args":{"command":command,"args":args}}))).await;
        let row = self.call("spawn", "bash").await;
        assert_eq!(row["await_mode"], "background");
        row
    }

    async fn terminal(&self, call: &Value, state: &str) {
        let query = format!(
            r#"{{AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}) {{lifecycle_state}}}}"#,
            escape_graphql_string(call["_docID"].as_str().unwrap())
        );
        wait_for_rows(
            &self.actors.fixture.access,
            "AgentToolCall",
            &query,
            Duration::from_secs(30),
            |rows| rows["AgentToolCall"][0]["lifecycle_state"] == state,
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_tool_success_returns_handle_and_wait_tool_returns_terminal_envelope() {
    let fixture = Fixture::new("live-background-wait").await;
    fixture.request("spawn-and-wait", "Call spawn_process with tool_name bash and args {\"command\":\"printf\",\"args\":[\"done\"]}. Use the returned tool_call_id to call wait_process. After it completes, acknowledge briefly.").await;
    let child = fixture.call("spawn-and-wait", "bash").await;
    fixture.terminal(&child, "completed").await;
    let waited = fixture
        .result(&fixture.call("spawn-and-wait", "wait_process").await)
        .await;
    assert_eq!(waited["status"], "completed");
    assert!(waited["result"].as_str().unwrap().contains("done"));
    fixture.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_controls_manage_same_principal_job_across_request_turns() {
    let fixture = Fixture::new("live-background-cross-turn").await;
    let child = fixture.spawn("printf", &["retained result"]).await;
    fixture
        .request(
            "wait-later",
            &format!(
                "Call wait_process with tool_call_id {}. Then acknowledge briefly.",
                child["tool_call_id"]
            ),
        )
        .await;
    let waited = fixture
        .result(&fixture.call("wait-later", "wait_process").await)
        .await;
    assert_eq!(waited["status"], "completed");
    assert!(waited["result"]
        .as_str()
        .unwrap()
        .contains("retained result"));
    fixture.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_tool_cancels_running_background_row_through_canonical_control_call() {
    let fixture = Fixture::new("live-background-cancel").await;
    let path = fixture._root.path().join("waiting.txt");
    let child = fixture.spawn("tail", &["-f", path.to_str().unwrap()]).await;
    fixture
        .request(
            "cancel",
            &format!(
                "Call cancel_process with tool_call_id {}. Then acknowledge briefly.",
                child["tool_call_id"]
            ),
        )
        .await;
    fixture.terminal(&child, "cancelled").await;
    fixture.runtime.shutdown().await;
}

async fn unknown_handle(tool: &str) {
    let fixture = Fixture::new(&format!("live-{tool}-unknown")).await;
    fixture.request("unknown-handle", &format!("Call {tool} with tool_call_id \"missing-background-handle\" even though it does not exist. Do not search for another handle or retry. After the error, acknowledge it briefly.")).await;
    let result = fixture
        .result(&fixture.call("unknown-handle", tool).await)
        .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["tool_name"], tool);
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("missing-background-handle"));
    fixture.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    unknown_handle("cancel_process").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    unknown_handle("wait_process").await;
}
