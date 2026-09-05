use std::sync::Arc;

use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::row::AgentRequestRow;
use tempfile::TempDir;

pub async fn boot_core() -> (Arc<ClientCore>, TempDir) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core starts");
    (Arc::new(core), tempdir)
}

pub async fn seed_standalone_fixture() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = boot_core().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_solo",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_solo",
            content: "standalone fixture",
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

pub async fn seed_cascade_fixture() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = boot_core().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_root",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_root",
            content: "cascade fixture root",
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

    let child_requests = r#"mutation {
        r_b91: create_AgentRequest(input: {
            request_id: "req_b91",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_b91",
            content: "child b91",
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

pub async fn seed_cascade_fixture_with_foreign_request() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = seed_cascade_fixture().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_foreign",
            agent_did: "did:test:other",
            behavior_id: "other-behavior",
            session_id: "sess_foreign",
            content: "foreign agent request",
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

pub async fn seed_cascade_fixture_with_foreign_linked_child() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = seed_cascade_fixture().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: {
            request_id: "req_foreign_linked",
            agent_did: "did:test:other",
            behavior_id: "other-behavior",
            session_id: "sess_foreign_linked",
            content: "foreign linked child request",
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

pub async fn fetch_request_row(core: &Arc<ClientCore>, request_id: &str) -> AgentRequestRow {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
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
    serde_json::from_value(row)
        .unwrap_or_else(|error| panic!("fetch_request_row: invalid request {request_id}: {error}"))
}
