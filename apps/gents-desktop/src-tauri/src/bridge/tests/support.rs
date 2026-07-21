use std::sync::Arc;

use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents::graphql::escape_graphql_string;
use tempfile::TempDir;

/// Minimal projection of an `AgentRequest` row used in interrupt tests.
pub(crate) struct AgentRequestRowLite {
    pub interrupt_requested_at: Option<String>,
}

pub(crate) async fn boot_core() -> (Arc<ClientCore>, TempDir) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core starts");
    (Arc::new(core), tempdir)
}

/// Seeds a single `AgentRequest` with no children — the standalone fixture.
pub(crate) async fn seed_standalone_fixture() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = boot_core().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_solo",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_solo",
            content: "standalone fixture",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:00:00Z",
            retry_count: 0
        }) { _docID }
    }"#;

    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed standalone AgentRequest failed: {:?}",
        response.errors
    );

    (core, tmp)
}

/// Seeds the "5 children across 2 deployments" cascade fixture.
///
/// Parent: `req_root` (processing)
/// Children via AgentToolCall edges:
///   tc_1 → req_b91  processing  cancel_policy=cascade   → WillInterrupt
///   tc_2 → req_b92  claimed     cancel_policy=cascade   → WillInterrupt
///   tc_3 → req_b93  processing  cancel_policy=detach    → WillDetach
///   tc_4 → req_c01  processing  cancel_policy=cascade   → WillInterrupt
///   tc_5 → req_c02  processing  cancel_policy=(omitted) → UnknownPolicy
///   tc_6 → req_a17_old completed cancel_policy=cascade  → AlreadyTerminal
pub(crate) async fn seed_cascade_fixture() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = boot_core().await;

    // ── Root request ──────────────────────────────────────────────────────────
    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_root",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_root",
            content: "cascade fixture root",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:00:00Z",
            retry_count: 0
        }) { _docID }
    }"#;
    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed req_root failed: {:?}",
        response.errors
    );

    // ── Child requests ────────────────────────────────────────────────────────
    let child_requests = r#"mutation {
        r_b91: create_AgentRequest(input: {
            request_id: "req_b91",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_b91",
            content: "child b91",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:01:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_1"
        }) { _docID }
        r_b92: create_AgentRequest(input: {
            request_id: "req_b92",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_b92",
            content: "child b92",
            status: "claimed",
            lifecycle_state: "claimed",
            backend_id: "",
            created_at: "2026-05-20T00:02:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_2"
        }) { _docID }
        r_b93: create_AgentRequest(input: {
            request_id: "req_b93",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_b93",
            content: "child b93",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:03:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_3"
        }) { _docID }
        r_c01: create_AgentRequest(input: {
            request_id: "req_c01",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_c01",
            content: "child c01",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:04:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_4"
        }) { _docID }
        r_c02: create_AgentRequest(input: {
            request_id: "req_c02",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_c02",
            content: "child c02",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:05:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_5"
        }) { _docID }
        r_a17: create_AgentRequest(input: {
            request_id: "req_a17_old",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_a17",
            content: "child a17 old (completed)",
            status: "completed",
            lifecycle_state: "completed",
            backend_id: "",
            created_at: "2026-05-20T00:06:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_6"
        }) { _docID }
    }"#;
    let response = core.node().execute(child_requests).await;
    assert!(
        !response.has_errors(),
        "seed child AgentRequests failed: {:?}",
        response.errors
    );

    // ── AgentToolCall edges on req_root ───────────────────────────────────────
    let tool_calls = r#"mutation {
        tc1: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_1",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 1,
            tool_name: "summarize",
            tool_call_id: "tc_1",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:01:00Z",
            await_mode: "background",
            cancel_policy: "cascade",
            child_request_id: "req_b91"
        }) { _docID }
        tc2: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_2",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 2,
            tool_name: "index_repo",
            tool_call_id: "tc_2",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:02:00Z",
            await_mode: "background",
            cancel_policy: "cascade",
            child_request_id: "req_b92"
        }) { _docID }
        tc3: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_3",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 3,
            tool_name: "qa_pass",
            tool_call_id: "tc_3",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:03:00Z",
            await_mode: "background",
            cancel_policy: "detach",
            child_request_id: "req_b93"
        }) { _docID }
        tc4: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_4",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 4,
            tool_name: "summarize_caselaw",
            tool_call_id: "tc_4",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:04:00Z",
            await_mode: "background",
            cancel_policy: "cascade",
            child_request_id: "req_c01"
        }) { _docID }
        tc5: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_5",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 5,
            tool_name: "classify_docs",
            tool_call_id: "tc_5",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:05:00Z",
            await_mode: "background",
            child_request_id: "req_c02"
        }) { _docID }
        tc6: create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_6",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 6,
            tool_name: "earlier_turn",
            tool_call_id: "tc_6",
            args: "{}",
            result: "done",
            status: "completed",
            lifecycle_state: "completed",
            started_at: "2026-05-20T00:06:00Z",
            await_mode: "foreground",
            cancel_policy: "cascade",
            child_request_id: "req_a17_old"
        }) { _docID }
    }"#;
    let response = core.node().execute(tool_calls).await;
    assert!(
        !response.has_errors(),
        "seed AgentToolCalls failed: {:?}",
        response.errors
    );

    (core, tmp)
}

/// Seeds the cascade fixture PLUS one unlinked `AgentRequest` owned by
/// `did:test:other` to verify that walks only follow bridge edges.
pub(crate) async fn seed_cascade_fixture_with_foreign_request() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = seed_cascade_fixture().await;

    // Seed one extra request owned by a different agent DID. It references the
    // root lineage fields, but no AgentToolCall points at it, so the walk must
    // not include it.
    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_foreign",
            agent_did: "did:test:other",
            behavior_id: "other-behavior",
            session_id: "sess_foreign",
            content: "foreign agent request",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:07:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_1"
        }) { _docID }
    }"#;
    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed foreign AgentRequest failed: {:?}",
        response.errors
    );

    (core, tmp)
}

/// Seeds the cascade fixture PLUS one cascade-linked `AgentRequest` owned by
/// `did:test:other`, matching live cross-deployment subagent edges.
pub(crate) async fn seed_cascade_fixture_with_foreign_linked_child() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = seed_cascade_fixture().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_foreign_linked",
            agent_did: "did:test:other",
            behavior_id: "other-behavior",
            session_id: "sess_foreign_linked",
            content: "foreign linked child request",
            status: "processing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:07:00Z",
            retry_count: 0,
            caused_by_parent_request_id: "req_root",
            caused_by_parent_tool_call_id: "tc_foreign"
        }) { _docID }

        create_AgentToolCall(input: {
            tool_call_key: "sess_root:tc_foreign",
            request_id: "req_root",
            session_id: "sess_root",
            message_sequence: 7,
            tool_name: "remote_subagent",
            tool_call_id: "tc_foreign",
            args: "{}",
            result: "",
            status: "called",
            lifecycle_state: "running",
            started_at: "2026-05-20T00:07:00Z",
            await_mode: "background",
            cancel_policy: "cascade",
            child_request_id: "req_foreign_linked"
        }) { _docID }
    }"#;
    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed foreign linked child failed: {:?}",
        response.errors
    );

    (core, tmp)
}

/// Fetches a single `AgentRequest` row by `request_id` and returns a
/// `AgentRequestRowLite` for assertions in interrupt tests. Panics if the
/// request is not found.
pub(crate) async fn fetch_request_row(
    core: &Arc<ClientCore>,
    request_id: &str,
) -> AgentRequestRowLite {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
            }}
        }}"#
    );

    let response = core.node().execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch_request_row query failed for {request_id}: {:?}",
        response.errors
    );

    let data = response.data.unwrap_or(serde_json::Value::Null);
    let row = data
        .get("AgentRequest")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or_else(|| panic!("fetch_request_row: request {request_id} not found"));

    let interrupt_requested_at = row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    AgentRequestRowLite {
        interrupt_requested_at,
    }
}
