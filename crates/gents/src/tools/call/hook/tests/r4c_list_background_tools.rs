use gents::defra_node::EmbeddedNode;
use gents::inference::llm::tool::BoxFuture;
use gents::inference::llm::tool::ToolDefinition;
use gents::inference::llm::tool::{ToolDyn, ToolError};
use gents::inference::llm::ToolCallHookAction;
use gents::store::graphql::escape_graphql_string;
use gents::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use serde_json::{json, Value};

use super::r4c_private_support::{
    accepted_call, assert_accepted_control_rows, bind_accepted_request,
};
use crate::support::test_db;

#[path = "background_wait_wakes.rs"]
mod background_wait_wakes;

#[path = "background_wait_profile.rs"]
mod background_wait_profile;

struct StaticTool {
    name: &'static str,
    result: &'static str,
}

impl ToolDyn for StaticTool {
    fn name(&self) -> String {
        self.name.to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.result.to_string()) })
    }
}

struct PendingTool;

struct GatedTool(std::sync::Arc<tokio::sync::Semaphore>);

impl ToolDyn for GatedTool {
    fn name(&self) -> String {
        "gated_tool".to_owned()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "gated_tool".to_owned(),
                description: "gated tool".to_owned(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async {
            let _permit = self.0.acquire().await.unwrap();
            Ok("wake-completed".to_owned())
        })
    }
}

struct SilentChanges {
    waiting: std::sync::Arc<tokio::sync::Notify>,
    closed: bool,
}

impl futures::Stream for SilentChanges {
    type Item = events::DocumentChangeBatch;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.waiting.notify_one();
        if self.closed {
            std::task::Poll::Ready(None)
        } else {
            std::task::Poll::Pending
        }
    }
}

async fn background_wait_recovers_without_notification(closed: bool) {
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let (db, hook, session_id, request_id) = setup_hook(
        if closed {
            "wait-closed-feed"
        } else {
            "wait-silent-feed"
        },
        registry(vec![Box::new(GatedTool(gate.clone()))], &["gated_tool"]),
    )
    .await;
    let handle = background_tool(&hook, "gated-background", "gated_tool").await;
    let caller = crate::tools::background::status::ProcessControlScope {
        request_id,
        session_id,
        node_did: db.node_identity.did().to_owned(),
        requester_did: Some(db.node_identity.did().to_owned()),
    };
    let waiting = std::sync::Arc::new(tokio::sync::Notify::new());
    let changes = SilentChanges {
        waiting: waiting.clone(),
        closed,
    };
    let now = chrono::Utc::now();
    let wait = hook.await_background_tool_observing(
        &caller,
        handle["tool_call_id"].as_str().unwrap(),
        now + chrono::Duration::minutes(1),
        now + chrono::Duration::seconds(30),
        changes,
    );
    let complete = async {
        waiting.notified().await;
        gate.add_permits(1);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(wait, complete)
    })
    .await
    .expect("durable completion must be observed before the caller's wait expires");
    let result: Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["result"], "wake-completed");
    assert_eq!(result["error"], Value::Null);
}

#[tokio::test]
async fn background_wait_recovers_after_a_lost_notification() {
    background_wait_recovers_without_notification(false).await;
}

#[tokio::test]
async fn background_wait_recovers_after_the_change_feed_closes() {
    background_wait_recovers_without_notification(true).await;
}

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".to_string(),
                description: "test tool".to_string(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn setup_hook(
    test_name: &str,
    registry: BackgroundToolRegistry,
) -> (crate::support::TestDb, DefraSessionHook, String, String) {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    let did = db.node_identity.did();
    gents::session::ensure_session_with_agent_id_and_requester_did(
        &db.node,
        &session_id,
        did,
        "r4c-background-tools",
        Some(did),
    )
    .await
    .unwrap();

    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        did,
        Some(did),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    bind_accepted_request(
        &db,
        &hook,
        "r4c-background-tools",
        &request_id,
        &session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    (db, hook, session_id, request_id)
}

fn registry(tools: Vec<Box<dyn ToolDyn>>, allowlist: &[&str]) -> BackgroundToolRegistry {
    BackgroundToolRegistry::from_tools(
        tools,
        &allowlist
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
    )
}

async fn background_tool(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_name: &str,
) -> Value {
    background_tool_with_args(hook, internal_call_id, tool_name, json!({})).await
}

async fn background_tool_with_args(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_name: &str,
    args: Value,
) -> Value {
    skip_reason_json(
        accepted_call(
            hook,
            "spawn_process",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({"tool_name": tool_name, "args": args}).to_string(),
        )
        .await,
    )
}

async fn wait_tool(hook: &DefraSessionHook, internal_call_id: &str, tool_call_id: &str) -> Value {
    skip_reason_json(
        accepted_call(
            hook,
            "wait_process",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    )
}

/// wait_process with an explicit bounded timeout_secs (no polling loop):
/// the failure diagnostic path uses 1s so an unfinished child is reported
/// as wait_timeout with its current evidence instead of blocking 30s.
async fn wait_tool_with_timeout(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_call_id: &str,
    timeout_secs: u64,
) -> Value {
    skip_reason_json(
        accepted_call(
            hook,
            "wait_process",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({ "tool_call_id": tool_call_id, "timeout_secs": timeout_secs }).to_string(),
        )
        .await,
    )
}

async fn read_tool(hook: &DefraSessionHook, internal_call_id: &str, tool_call_id: &str) -> Value {
    skip_reason_json(
        accepted_call(
            hook,
            "read_process",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({ "tool_call_id": tool_call_id, "offset": 0, "max_tokens": 64 }).to_string(),
        )
        .await,
    )
}

/// Test-only persisted-row snapshot, ACP-scoped to this test's session and
/// sanitized: state tokens, await_mode token, and booleans only (no raw
/// timestamps or payloads). Distinguishes at failure time whether the
/// spawned row (a) was never persisted / has a different identity, (b)
/// exists but is stuck pending, (c) exists and failed, or (d) is running
/// with unexpected bytes.
async fn persisted_row_snapshot(hook: &DefraSessionHook, handle_tool_call_id: &str) -> Value {
    let session_id = hook.session_id().await.expect("session id");
    let session = crate::store::graphql::escape_graphql_string(&session_id);
    let response = hook
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ _docID: asc }}) {{ tool_call_id tool_name lifecycle_state await_mode started_at completed_at }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "persisted row snapshot query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .expect("persisted row snapshot query returned no data")
        .get("AgentToolCall")
        .expect("AgentToolCall collection missing from snapshot response")
        .as_array()
        .expect("AgentToolCall snapshot is not an array")
        .clone();
    let sanitized: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "tool_call_id_matches_handle": row["tool_call_id"].as_str() == Some(handle_tool_call_id),
                "tool_name": row["tool_name"].as_str(),
                "lifecycle_state": row["lifecycle_state"].as_str(),
                "await_mode": row["await_mode"].as_str(),
                "has_started_at": row["started_at"].as_str().is_some(),
                "has_completed_at": row["completed_at"].as_str().is_some(),
            })
        })
        .collect();
    json!({ "row_count": rows.len(), "rows": sanitized })
}

async fn list_background_tools(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    args: Value,
) -> Value {
    skip_reason_json(
        accepted_call(
            hook,
            "list_processes",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &args.to_string(),
        )
        .await,
    )
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_name: {{ _eq: "{tool_name}" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map_or(0, Vec::len)
}

async fn create_foreground_tool_call(hook: &DefraSessionHook) {
    let mut lifecycle = super::accepted_hook_tool_lifecycle(
        hook,
        "foreground-call",
        "foreground_tool",
        "{}",
        chrono::Utc::now() + chrono::Duration::minutes(5),
        gents::tools::call::lifecycle::AwaitMode::Foreground,
    )
    .await;
    lifecycle.start_running().await.unwrap();
    lifecycle.complete("foreground result").await.unwrap();
}

#[tokio::test]
async fn list_background_tools_returns_running_bg_tools() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-list-bg-running",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let handle_a = background_tool(&hook, "bg-a", "slow_tool").await;
    let handle_b = background_tool(&hook, "bg-b", "slow_tool").await;

    let result = list_background_tools(&hook, "list-running", json!({})).await;
    let entries = result["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    let ids = entries
        .iter()
        .map(|entry| entry["tool_call_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&handle_a["tool_call_id"].as_str().unwrap()));
    assert!(ids.contains(&handle_b["tool_call_id"].as_str().unwrap()));
    for entry in entries {
        assert_eq!(entry["tool_name"].as_str(), Some("slow_tool"));
        assert_eq!(entry["await_mode"].as_str(), Some("background"));
        assert_eq!(entry["status"].as_str(), Some("running"));
        assert_eq!(entry["stdout_bytes"].as_u64(), Some(0));
        assert_eq!(entry["stderr_bytes"].as_u64(), Some(0));
    }
}

#[tokio::test]
async fn list_background_tools_reports_running_live_output_bytes() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = gents::ToolSet::builder()
        .bash_unrestricted(tempdir.path())
        .build()
        .build_native_tools()
        .expect("native tools should build");
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-list-bg-live-output",
        registry(tools, &["bash_unrestricted"]),
    )
    .await;
    // Byte semantics under test (explicit contract):
    // - `stdout_bytes` / `stderr_bytes` are the DEPRECATED COMBINED LEGACY
    //   fields: they carry the length of the CANONICAL rendering of all
    //   output observed so far (never per-channel accounting; stderr is
    //   hard-coded 0). At base 02351da4d the list handler counted exactly
    //   the same thing (git show 02351da4d:crates/gents/src/tools/background/status.rs
    //   L171-176: `(output.len() as u64, 0)` over `canonical_tool_output`).
    // - `available_bytes` / `preview` expose the same canonical evidence
    //   and coordinate with read_process, which slices the identical
    //   rendering (agreement asserted below at the mid-run observation).
    // Base behavior was timing-dependent: an early poll caught the 4-byte
    // "live" Open observation while a poll after completion saw the full
    // terminal rendering (53 in the failing run). The child is now gated
    // by a release FILE (portable: no flock — separate-process flock -u
    // cannot release the child fd, and macOS lacks flock): it prints
    // "live", then polls for the release file's existence in a bounded
    // loop, so the mid-run observation is deterministic. The parent writes
    // the release file only after its assertions; if an assertion fails,
    // the bounded loop expires and the child's tool timeout reaps it. No
    // post-release byte arithmetic is asserted.
    let release_path = tempdir.path().join("release-gate");
    std::fs::create_dir_all(tempdir.path()).expect("tempdir exists");
    // POSIX-safe single-quoted path (tempdir paths contain no single
    // quotes; assert to keep that invariant load-bearing).
    assert!(!release_path.to_string_lossy().contains('\''));
    // Simple approved executable invocation per TPM direction: the gate
    // loop lives in a script file written by the test, invoked as
    // command=/bin/sh with the script path as a separate argument. This
    // avoids the /bin/sh -lc inline compound-string path (validator treats
    // an empty `args` as a shell string; the identical-shape sleep command
    // worked at combinedc3984a342, so the inline while compound is the
    // remaining variable). The script content is the same gate: print
    // live, bounded wait for the release file, print done.
    let script_path = tempdir.path().join("gate-child.sh");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf live\ni=0\nwhile [ ! -e '{release}' ] && [ \"$i\" -lt 200 ]; do\n  sleep 0.05\n  i=$((i+1))\ndone\nprintf done\n",
            release = release_path.display()
        ),
    )
    .expect("write gate script");
    let handle = background_tool_with_args(
        &hook,
        "bg-live-output",
        "bash_unrestricted",
        json!({
            "command": "/bin/sh",
            "args": [script_path.to_string_lossy().to_string()],
            "timeout_secs": 20
        }),
    )
    .await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();

    // First observation happens while the child is gate-blocked after
    // printing "live": exactly 4 canonical bytes are observable.
    //
    // Root-cause instrumentation (closeout-status-accounting failure at
    // line 330): the previous loop ran only 40x50ms (2s) — shorter than
    // the child's own gate budget — and on exhaustion the code asserted
    // against an empty json!({}), discarding every actual response. The
    // loop now spans the gate window (60x100ms = 6s < 10s gate ceiling)
    // and, if no entry matches, panics with the LAST captured list
    // response plus the tracked per-attempt observation of this
    // tool_call_id (status and stdout_bytes over time), so failures are
    // diagnosed from evidence, not inference.
    let mut entry = json!({});
    let mut last_response = json!(null);
    let mut observations: Vec<(String, u64, String)> = Vec::new();
    for attempt in 0..60 {
        let result =
            list_background_tools(&hook, &format!("list-live-output-{attempt}"), json!({})).await;
        let entries = result["entries"].as_array().expect("entries");
        if let Some(candidate) = entries
            .iter()
            .find(|entry| entry["tool_call_id"].as_str() == Some(tool_call_id))
        {
            observations.push((
                candidate["status"]
                    .as_str()
                    .unwrap_or("<missing>")
                    .to_string(),
                candidate["stdout_bytes"].as_u64().unwrap_or(u64::MAX),
                candidate["available_bytes"]
                    .as_u64()
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "<missing>".to_string()),
            ));
        }
        last_response = result.clone();
        if let Some(matched) = entries
            .iter()
            .find(|entry| entry["tool_call_id"].as_str() == Some(tool_call_id))
            .filter(|entry| entry["status"].as_str() == Some("running"))
            .filter(|entry| entry["stdout_bytes"].as_u64().unwrap_or_default() == 4)
        {
            entry = matched.clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Failure diagnostics run BEFORE any observations/mid-run asserts and
    // use an explicit found-boolean (entry starts as json!({}), so is_null
    // can never be true - reviewer-caught bug in 452e0c4a).
    let found = !entry.as_object().is_some_and(|o| o.is_empty());
    if !found {
        let waited_failure = wait_tool_with_timeout(
            &hook,
            "wait-failure",
            tool_call_id,
            1, // explicit timeout_secs=1: bounded, not 10 loops at the 30s default
        )
        .await;
        let read_failure = read_tool(&hook, "read-failure", tool_call_id).await;
        panic!(
            "gate window closed with an empty observation list; \
             tool_call_id {tool_call_id} never appeared in any list response; \
             last response: {last_response}; \
             persisted-row snapshot: {}; \
             wait envelope (timeout_secs=1): {}; \
             bounded read (min 64 tokens): {}",
            serde_json::to_string(&persisted_row_snapshot(&hook, tool_call_id).await).unwrap(),
            serde_json::to_string(&waited_failure).unwrap(),
            read_failure["output"].as_str().unwrap_or("<no output>")
        );
    }
    assert!(
        !observations.is_empty(),
        "tool_call_id {tool_call_id} never appeared in any list response; \
         last response: {last_response}; \
         persisted-row snapshot: {}",
        serde_json::to_string(&persisted_row_snapshot(&hook, tool_call_id).await).unwrap()
    );
    assert_eq!(entry["tool_name"].as_str(), Some("bash_unrestricted"),
        "no running observation with stdout_bytes==4 within the gate window; per-attempt status/bytes: {observations:?}; last response: {last_response}");
    assert_eq!(entry["status"].as_str(), Some("running"));
    assert_eq!(
        entry["stdout_bytes"].as_u64(),
        Some(4),
        "canonical bytes observed while the child is gate-blocked after printing 'live'"
    );
    assert_eq!(entry["stderr_bytes"].as_u64(), Some(0));
    assert_eq!(
        entry["available_bytes"].as_u64(),
        Some(4),
        "available_bytes agrees with the deprecated legacy count on the same canonical rendering"
    );
    assert_eq!(entry["preview"].as_str(), Some("live"));

    // Coordinate with read_process on the identical canonical evidence.
    let read = read_tool(&hook, "read-live-output", tool_call_id).await;
    assert_eq!(read["total_bytes"].as_u64(), Some(4));
    assert_eq!(read["output"].as_str(), Some("live"));

    // Release the gate: the child observes the file, prints "done", and
    // completes. Its exit status is asserted via wait_process; no byte
    // count is asserted for the terminal state.
    tokio::fs::write(&release_path, b"release")
        .await
        .expect("write release file");

    let waited = wait_tool(&hook, "wait-live-output", tool_call_id).await;
    assert_eq!(
        waited["status"].as_str(),
        Some("completed"),
        "child failed at terminal state; canonical terminal envelope: {}",
        serde_json::to_string(&waited).unwrap()
    );
}

#[tokio::test]
async fn list_background_tools_rejects_sibling_requests() {
    let db = test_db("r4c-list-bg-sibling").await;
    let (hook_1, _session_1, _request_1) = setup_hook_on_db(
        &db,
        "parent-one",
        "session-one",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let (hook_2, _session_2, _request_2) = setup_hook_on_db(
        &db,
        "parent-two",
        "session-two",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    background_tool(&hook_2, "sibling-bg", "slow_tool").await;

    let result = list_background_tools(&hook_1, "list-sibling", json!({})).await;
    assert!(result["entries"].as_array().unwrap().is_empty());
}

async fn setup_hook_on_db(
    db: &crate::support::TestDb,
    request_id: &str,
    session_id: &str,
    registry: BackgroundToolRegistry,
) -> (DefraSessionHook, String, String) {
    let did = db.node_identity.did();
    gents::session::ensure_session_with_agent_id_and_requester_did(
        &db.node,
        session_id,
        did,
        "r4c-background-tools",
        Some(did),
    )
    .await
    .unwrap();
    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        session_id,
        did,
        Some(did),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    bind_accepted_request(
        db,
        &hook,
        "r4c-background-tools",
        request_id,
        session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    (hook, session_id.to_string(), request_id.to_string())
}

#[tokio::test]
async fn list_background_tools_excludes_foreground_calls() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-list-bg-foreground",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    create_foreground_tool_call(&hook).await;
    background_tool(&hook, "bg-visible", "slow_tool").await;

    let result = list_background_tools(&hook, "list-foreground", json!({})).await;
    let entries = result["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["tool_name"].as_str(), Some("slow_tool"));
}

#[tokio::test]
async fn list_background_tools_status_filter() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-list-bg-status",
        registry(
            vec![Box::new(StaticTool {
                name: "complete_tool",
                result: "terminal output",
            })],
            &["complete_tool"],
        ),
    )
    .await;
    let handle = background_tool(&hook, "bg-terminal", "complete_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();
    let waited = wait_tool(&hook, "wait-terminal", tool_call_id).await;
    assert_eq!(waited["status"].as_str(), Some("completed"));

    let running =
        list_background_tools(&hook, "list-status-running", json!({"status": "running"})).await;
    assert_eq!(running["entries"].as_array().unwrap().len(), 0);

    let terminal =
        list_background_tools(&hook, "list-status-terminal", json!({"status": "terminal"})).await;
    assert_eq!(terminal["entries"].as_array().unwrap().len(), 1);
    assert_eq!(terminal["entries"][0]["status"].as_str(), Some("completed"));
    assert_eq!(
        terminal["entries"][0]["stdout_bytes"].as_u64(),
        Some("terminal output".len() as u64)
    );
}

#[tokio::test]
async fn list_background_tools_limit_truncates() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-list-bg-limit",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    for index in 0..5 {
        background_tool(&hook, &format!("bg-limit-{index}"), "slow_tool").await;
    }

    let result = list_background_tools(&hook, "list-limit", json!({"limit": 3})).await;
    assert_eq!(result["entries"].as_array().unwrap().len(), 3);
    assert_eq!(result["truncated"].as_bool(), Some(true));
}

#[tokio::test]
async fn list_background_tools_keeps_one_accepted_parent_control_row() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r4c-list-bg-no-row",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    background_tool(&hook, "bg-no-row", "slow_tool").await;
    let _ = list_background_tools(&hook, "list-no-row", json!({})).await;

    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "list_processes").await,
        1
    );
    assert_accepted_control_rows(&hook, "list_processes", 1).await;
}
