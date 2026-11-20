use crate::client_runtime::ClientRuntime;
use crate::interrupt::interrupt_request;
use crate::tests::support::{
    reject_admission, served_desktop, served_desktop_pair, submit_request,
};
use crate::types::DesktopInterruptRequest;
use gents_store::graphql::escape_graphql_string;

fn user_cancel(request_id: &str, node: Option<&str>) -> DesktopInterruptRequest {
    DesktopInterruptRequest {
        request_id: request_id.into(),
        node_did: node.map(str::to_owned),
        cause: "userCancelled".into(),
    }
}

async fn latch(core: &ClientRuntime, node: &str, request: &str) -> Option<String> {
    core.operator_access(node).unwrap().execute(&format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ interrupt_requested_at }} }}",
        escape_graphql_string(request),
    )).await.unwrap()["data"]["AgentRequest"][0]["interrupt_requested_at"]
        .as_str().map(str::to_owned)
}

#[tokio::test]
async fn interrupt_uses_hosting_runtime_and_preserves_other_requests() {
    let served = served_desktop().await;
    let (core, node) = (&served.core, served.node_did());
    submit_request(
        core,
        &served.runtime,
        &served.agent,
        "parent-session",
        "parent",
    )
    .await;
    submit_request(
        core,
        &served.runtime,
        &served.agent,
        "child-session",
        "child",
    )
    .await;
    let first = interrupt_request(core, &user_cancel("parent", Some(node)))
        .await
        .unwrap();
    assert!(first.accepted);
    assert!(!first.already_interrupted);
    assert_eq!(
        first.interrupt_requested_at,
        latch(core, node, "parent").await
    );
    assert!(first.interrupt_requested_at.is_some());
    assert!(latch(core, node, "child").await.is_none());
    let second = interrupt_request(core, &user_cancel("parent", Some(node)))
        .await
        .unwrap();
    assert!(second.already_interrupted);
    assert_eq!(second.interrupt_requested_at, first.interrupt_requested_at);
    let done = submit_request(core, &served.runtime, &served.agent, "done-session", "done").await;
    reject_admission(&served.runtime, &served.agent, &done, "fixture rejection").await;
    let finished = interrupt_request(core, &user_cancel("done", Some(node)))
        .await
        .unwrap();
    assert!(finished.accepted && finished.already_interrupted);
    assert!(finished.interrupt_requested_at.is_none());
    assert!(latch(core, node, "done").await.is_none());
    served.shutdown().await;
}

#[tokio::test]
async fn interrupt_refuses_non_user_causes_and_an_unselected_runtime() {
    let (core, _directory) = crate::tests::support::boot_core().await;
    let mut invalid = user_cancel("done", Some("did:test:node"));
    invalid.cause = "deadline".into();
    assert!(interrupt_request(&core, &invalid)
        .await
        .unwrap_err()
        .message
        .contains("userCancelled"));
    assert!(
        interrupt_request(&core, &user_cancel("done", None))
            .await
            .unwrap_err()
            .message
            .contains("Select the hosting runtime"),
        "the owner never falls back to the core's shared selection"
    );
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_routes_same_logical_id_to_the_explicit_runtime() {
    let (served, second, second_agent) = served_desktop_pair().await;
    let core = &served.core;
    let (first_did, second_did) = (served.node_did(), second.node_did());
    submit_request(core, &served.runtime, &served.agent, "one", "same-id").await;
    submit_request(core, &second, &second_agent, "two", "same-id").await;
    interrupt_request(core, &user_cancel("same-id", Some(second_did)))
        .await
        .unwrap();
    assert!(latch(core, first_did, "same-id").await.is_none());
    assert!(latch(core, second_did, "same-id").await.is_some());
    second.shutdown().await;
    served.shutdown().await;
}

#[tokio::test]
async fn interrupt_refuses_a_missing_runtime_without_using_the_client_store() {
    let (core, _directory) = crate::tests::support::boot_core().await;
    assert!(!core
        .node()
        .get_all_collection_versions()
        .await
        .unwrap()
        .iter()
        .any(|collection| collection.name == "AgentRequest"));
    assert!(
        interrupt_request(&core, &user_cancel("local-only", Some("did:test:missing")))
            .await
            .is_err()
    );
    core.shutdown().await.unwrap();
}
