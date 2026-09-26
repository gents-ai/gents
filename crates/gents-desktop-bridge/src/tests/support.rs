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
        create_AgentRequest(input: { purpose: "normal",
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

/// Two sessions of one agent: `sess_parent`'s request `req_parent` started
/// `sess_child` with `create_session` (`req_child`), then messaged it again
/// (`req_child_2`); a third session on another agent was started by it too.
/// Returns the core and `req_parent`'s document id.
pub async fn seed_provenance_fixture() -> (Arc<ClientCore>, TempDir, String) {
    let (core, tmp) = boot_core().await;

    let parent = r#"mutation {
        create_AgentRequest(input: { purpose: "normal",
            request_id: "req_parent",
            agent_did: "did:test:operator",
            behavior_id: "lead",
            session_id: "sess_parent",
            content: "coordinate",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:00:00Z",
            retry_count: 0
        }) { _docID }
    }"#;
    let response = core.node().execute(parent).await;
    assert!(
        !response.has_errors(),
        "seed req_parent failed: {:?}",
        response.errors
    );
    let response = core
        .node()
        .execute(r#"{ AgentRequest(filter: { request_id: { _eq: "req_parent" } }) { _docID } }"#)
        .await;
    let parent_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.pointer("/AgentRequest/0/_docID"))
        .and_then(|value| value.as_str())
        .expect("req_parent doc id")
        .to_owned();

    let caused = format!(
        r#"mutation {{
        c1: create_AgentRequest(input: {{ purpose: "normal",
            request_id: "req_child",
            agent_did: "did:test:operator",
            behavior_id: "researcher",
            session_id: "sess_child",
            content: "look into it",
            lifecycle_state: "completed",
            backend_id: "",
            created_at: "2026-05-20T00:01:00Z",
            retry_count: 0,
            subagent_depth: 1,
            caused_by_parent_request_id: "req_parent",
            caused_by_parent_request_doc_id: "{parent_doc_id}",
            caused_by_parent_tool_call_id: "tc_start"
        }}) {{ _docID }}
        c2: create_AgentRequest(input: {{ purpose: "normal",
            request_id: "req_child_2",
            agent_did: "did:test:operator",
            behavior_id: "researcher",
            session_id: "sess_child",
            content: "one more thing",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:02:00Z",
            retry_count: 0,
            subagent_depth: 1,
            caused_by_parent_request_id: "req_parent",
            caused_by_parent_request_doc_id: "{parent_doc_id}",
            caused_by_parent_tool_call_id: "tc_message"
        }}) {{ _docID }}
        c3: create_AgentRequest(input: {{ purpose: "normal",
            request_id: "req_peer",
            agent_did: "did:test:other",
            behavior_id: "reviewer",
            session_id: "sess_peer",
            content: "review it",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:03:00Z",
            retry_count: 0,
            subagent_depth: 1,
            caused_by_parent_request_id: "req_parent",
            caused_by_parent_request_doc_id: "{parent_doc_id}",
            caused_by_parent_tool_call_id: "tc_peer"
        }}) {{ _docID }}
        unrelated: create_AgentRequest(input: {{ purpose: "normal",
            request_id: "req_unrelated",
            agent_did: "did:test:operator",
            behavior_id: "lead",
            session_id: "sess_unrelated",
            content: "something else",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:04:00Z",
            retry_count: 0
        }}) {{ _docID }}
    }}"#
    );
    let response = core.node().execute(&caused).await;
    assert!(
        !response.has_errors(),
        "seed caused requests failed: {:?}",
        response.errors
    );

    (core, tmp, parent_doc_id)
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
