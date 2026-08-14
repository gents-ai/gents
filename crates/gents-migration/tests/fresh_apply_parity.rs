//! Guard for #1123/#1125: client-authored (conversation-plane) collections
//! MUST remain fresh-apply compatible. A paired client mints its store from
//! a collection's *current* SDL with no server history — one `add_schema`
//! call, a genesis version whose DAG-CBOR block has empty `heads`. If a
//! collection in `CLIENT_AUTHORED_COLLECTIONS` ever gains a post-baseline
//! `MigrationStep::PatchVersioned` step (or its baseline pin drifts from the
//! fresh-apply root), the server's active version becomes a chain tip whose
//! CID structurally can never equal the client's genesis CID — DefraDB's
//! version CID hashes a DAG-CBOR block that includes `heads`, and a
//! chain-tip block's heads are never empty. See
//! `.superpowers/sdd/task-1123-report.md` for the three-way probe that
//! discovered this for `AgentRequest`, and PR #1125 for the fix.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_migration::{ensure_migrations, CLIENT_AUTHORED_COLLECTIONS};

async fn fresh_node() -> Arc<EmbeddedNode> {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = EmbeddedNode::builder()
        .data_path(dir.path())
        .build()
        .await
        .expect("build node");
    // Keep tempdir alive for the node lifetime by leaking (test process short).
    std::mem::forget(dir);
    Arc::new(node)
}

/// The live SDL a fresh client mints its store from for one client-authored
/// collection. Mirrors the mapping the deleted `schema_cid_probe.rs` probe
/// used (`gents_protocol::schemas::AGENT_ALL`, one `add_schema` call per
/// SDL, in registry order) — see the task-1123 report.
fn current_sdl(name: &str) -> &'static str {
    match name {
        n if n == gents_protocol::schemas::AGENT_REQUEST_NAME => {
            gents_protocol::schemas::AGENT_REQUEST
        }
        n if n == gents_protocol::schemas::AGENT_RESPONSE_NAME => {
            gents_protocol::schemas::AGENT_RESPONSE
        }
        n if n == gents_protocol::schemas::AGENT_MESSAGE_NAME => {
            gents_protocol::schemas::AGENT_MESSAGE
        }
        n if n == gents_protocol::schemas::AGENT_TOOL_CALL_NAME => {
            gents_protocol::schemas::AGENT_TOOL_CALL
        }
        n if n == gents_protocol::schemas::AGENT_TOOL_RESULT_NAME => {
            gents_protocol::schemas::AGENT_TOOL_RESULT
        }
        n if n == gents_protocol::schemas::AGENT_SESSION_NAME => {
            gents_protocol::schemas::AGENT_SESSION
        }
        n if n == gents_protocol::schemas::AGENT_CONVERSATION_NAME => {
            gents_protocol::schemas::AGENT_CONVERSATION
        }
        n if n == gents_protocol::schemas::COMPACTION_ENTRY_NAME => {
            gents_protocol::schemas::COMPACTION_ENTRY
        }
        n if n == gents_protocol::schemas::BEARER_PAIRING_READY_NAME => {
            gents_protocol::schemas::BEARER_PAIRING_READY
        }
        n if n == gents_protocol::schemas::PAIRING_BEARER_CLAIM_NAME => {
            gents_protocol::schemas::PAIRING_BEARER_CLAIM
        }
        n if n == gents_protocol::schemas::PEER_ENDPOINT_NAME => {
            gents_protocol::schemas::PEER_ENDPOINT
        }
        n if n == gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME => {
            gents_protocol::schemas::PERSONA_CONFIG_REQUEST
        }
        n if n == gents_protocol::schemas::AGENT_DIRECTORY_ENTRY_NAME => {
            gents_protocol::schemas::AGENT_DIRECTORY_ENTRY
        }
        other => panic!(
            "no live SDL mapped in fresh_apply_parity.rs for {other} — add one when \
             extending CLIENT_AUTHORED_COLLECTIONS"
        ),
    }
}

#[tokio::test]
async fn client_authored_collections_stay_fresh_apply_compatible() {
    // Node A: the real server boot path — full ensure_migrations chain
    // replay (baseline registration + every DEFAULT_STEPS entry).
    let server = fresh_node().await;
    ensure_migrations(server.as_ref())
        .await
        .expect("server ensure_migrations");

    // Node B: a fresh node standing in for a paired client, applying each
    // client-authored collection's CURRENT SDL directly — the same
    // single-call `add_schema` genesis path a mobile client's FFI takes
    // (`EmbeddedNode::add_schema` is the same code path as the FFI's
    // `add_schema`, per the task-1123 probe).
    let client = fresh_node().await;
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        client
            .add_schema(current_sdl(name))
            .await
            .unwrap_or_else(|error| panic!("client fresh-apply {name}: {error}"));
    }

    let mut mismatches = Vec::new();
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        let server_cv = server
            .get_collection(name)
            .expect("server get_collection")
            .unwrap_or_else(|| panic!("server missing {name}"));
        let client_cv = client
            .get_collection(name)
            .expect("client get_collection")
            .unwrap_or_else(|| panic!("client missing {name}"));
        if server_cv.version_id != client_cv.version_id {
            mismatches.push(format!(
                "{name}: server active version {} != client fresh-apply version {} — \
                 collection gained a post-baseline migration step or its baseline pin is \
                 stale — conversation-plane collections must be re-pinned to the \
                 fresh-apply CID, never chained; see #1123/#1125",
                server_cv.version_id, client_cv.version_id
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    server.shutdown().await;
    client.shutdown().await;
}
