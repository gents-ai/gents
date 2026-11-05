use crate::interrupt::interrupt_request;
use crate::tests::support::runtime_core;
use crate::types::DesktopInterruptRequest;
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;

fn user_cancel(request_id: &str, node: Option<&str>) -> DesktopInterruptRequest {
    DesktopInterruptRequest {
        request_id: request_id.into(),
        node_did: node.map(str::to_owned),
        cause: "userCancelled".into(),
    }
}

async fn seed(access: &ConfigAccess, node: &str, request: &str, session: &str, state: &str) {
    access.write("test.interrupt.seed", &format!(
        "mutation {{ create_AgentRequest(input: {{ purpose: \"normal\", request_id: \"{}\", node_did: \"{}\", agent_did: \"did:test:agent\", session_id: \"{}\", content: \"interrupt fixture\", lifecycle_state: \"{}\", created_at: \"2026-11-04T00:00:00Z\" }}) {{ _docID }} }}",
        escape_graphql_string(request), escape_graphql_string(node),
        escape_graphql_string(session), escape_graphql_string(state),
    )).await.unwrap();
}

async fn latch(core: &ClientCore, node: &str, request: &str) -> Option<String> {
    core.operator_access(node).unwrap().execute(&format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ interrupt_requested_at }} }}",
        escape_graphql_string(request),
    )).await.unwrap()["data"]["AgentRequest"][0]["interrupt_requested_at"]
        .as_str().map(str::to_owned)
}

#[tokio::test]
async fn interrupt_uses_hosting_runtime_and_preserves_other_requests() {
    let (core, runtime, _directory) = runtime_core().await;
    let node = runtime.node_identity_did().unwrap();
    let access = core.operator_access(node).unwrap();
    seed(&access, node, "parent", "parent-session", "processing").await;
    seed(&access, node, "child", "child-session", "processing").await;
    seed(
        &ConfigAccess::local(core.node_arc()),
        node,
        "parent",
        "stale",
        "completed",
    )
    .await;
    let first = interrupt_request(&core, &user_cancel("parent", None))
        .await
        .unwrap();
    assert!(first.accepted);
    assert!(!first.already_interrupted);
    assert_eq!(
        first.interrupt_requested_at,
        latch(&core, node, "parent").await
    );
    assert!(first.interrupt_requested_at.is_some());
    assert!(latch(&core, node, "child").await.is_none());
    let second = interrupt_request(&core, &user_cancel("parent", None))
        .await
        .unwrap();
    assert!(second.already_interrupted);
    assert_eq!(second.interrupt_requested_at, first.interrupt_requested_at);
    core.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_refuses_ambiguous_terminal_and_non_user_causes() {
    let (core, runtime, _directory) = runtime_core().await;
    let node = runtime.node_identity_did().unwrap();
    let access = core.operator_access(node).unwrap();
    seed(&access, node, "twins", "one", "processing").await;
    seed(&access, node, "twins", "two", "processing").await;
    assert!(interrupt_request(&core, &user_cancel("twins", None))
        .await
        .unwrap_err()
        .contains("ambiguous"));
    seed(&access, node, "done", "done-session", "completed").await;
    assert!(interrupt_request(&core, &user_cancel("done", None))
        .await
        .unwrap_err()
        .contains("already terminal"));
    assert!(latch(&core, node, "done").await.is_none());
    let mut invalid = user_cancel("done", None);
    invalid.cause = "deadline".into();
    assert!(interrupt_request(&core, &invalid)
        .await
        .unwrap_err()
        .contains("userCancelled"));
    core.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_routes_same_logical_id_to_the_explicit_runtime() {
    let (core, first, _one) = runtime_core().await;
    let (other_core, second, _two) = runtime_core().await;
    let first_did = first.node_identity_did().unwrap();
    let second_did = second.node_identity_did().unwrap();
    let endpoint = other_core.operator_graphql(second_did).unwrap();
    core.add_local_standard_peer_route_for_test(
        "Second",
        "127.0.0.1:56002/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
        second_did,
        endpoint.url(),
        "/tmp/test-second-runtime",
    )
    .await
    .unwrap();
    seed(
        &core.operator_access(first_did).unwrap(),
        first_did,
        "same-id",
        "one",
        "processing",
    )
    .await;
    seed(
        &core.operator_access(second_did).unwrap(),
        second_did,
        "same-id",
        "two",
        "processing",
    )
    .await;
    interrupt_request(&core, &user_cancel("same-id", Some(second_did)))
        .await
        .unwrap();
    assert!(latch(&core, first_did, "same-id").await.is_none());
    assert!(latch(&core, second_did, "same-id").await.is_some());
    core.shutdown().await.unwrap();
    other_core.shutdown().await.unwrap();
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_refuses_a_missing_runtime_without_using_the_client_store() {
    let (core, _directory) = crate::tests::support::boot_core().await;
    seed(
        &ConfigAccess::local(core.node_arc()),
        "did:test:missing",
        "local-only",
        "stale",
        "processing",
    )
    .await;
    assert!(
        interrupt_request(&core, &user_cancel("local-only", Some("did:test:missing")))
            .await
            .is_err()
    );
    core.shutdown().await.unwrap();
}
