use crate::client_runtime::ClientRuntime;
use crate::interrupt::interrupt_request;
use crate::tests::support::{reject_admission, served_desktop_pair, submit_request};
use crate::types::DesktopInterruptRequest;
use gents_store::graphql::escape_graphql_string;

fn user_cancel(request_id: &str, node: Option<&str>) -> DesktopInterruptRequest {
    DesktopInterruptRequest {
        request_id: request_id.into(),
        node_did: node.map(str::to_owned),
        cause: "userCancelled".into(),
    }
}

/// Whether the request was interrupted: before its claim, or by its
/// session's interrupt passing its claim.
async fn interrupted(core: &ClientRuntime, node: &str, request: &str) -> bool {
    let access = core.operator_access(node).unwrap();
    let row = access.execute(&format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ _docID lifecycle_state }} }}",
        escape_graphql_string(request),
    )).await.unwrap()["data"]["AgentRequest"][0].clone();
    row["lifecycle_state"] == "interrupted"
        || gents_session::request::interrupt::request_interrupted_at_by_doc_id_with_access(
            &access,
            row["_docID"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn interrupt_routes_by_hosting_runtime_and_refuses_invalid_or_missing_targets() {
    let (served, second, second_agent) = served_desktop_pair().await;
    let core = &served.core;
    let (first_did, second_did) = (served.node_did(), second.node_did());

    // Non-user causes, unselected runtimes, and missing runtimes are refused without local tables.
    assert!(!core
        .node()
        .get_all_collection_versions()
        .await
        .unwrap()
        .iter()
        .any(|collection| collection.name == "AgentRequest"));
    let mut invalid = user_cancel("done", Some("did:test:node"));
    invalid.cause = "deadline".into();
    assert!(interrupt_request(core, &invalid)
        .await
        .unwrap_err()
        .message
        .contains("userCancelled"));
    assert!(
        interrupt_request(core, &user_cancel("done", None))
            .await
            .unwrap_err()
            .message
            .contains("Select the hosting runtime"),
        "the owner never falls back to the core's shared selection"
    );
    assert!(
        interrupt_request(core, &user_cancel("local-only", Some("did:test:missing")))
            .await
            .is_err()
    );

    // Interrupting on the hosting runtime preserves sibling requests, is idempotent, and skips terminal ones.
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
    let first = interrupt_request(core, &user_cancel("parent", Some(first_did)))
        .await
        .unwrap();
    assert!(first.accepted);
    assert!(!first.already_interrupted);
    assert!(interrupted(core, first_did, "parent").await);
    assert!(!interrupted(core, first_did, "child").await);
    let duplicate = interrupt_request(core, &user_cancel("parent", Some(first_did)))
        .await
        .unwrap();
    assert!(duplicate.already_interrupted);
    assert_eq!(duplicate.interrupted_at, first.interrupted_at);
    let done = submit_request(core, &served.runtime, &served.agent, "done-session", "done").await;
    reject_admission(&served.runtime, &served.agent, &done, "fixture rejection").await;
    let finished = interrupt_request(core, &user_cancel("done", Some(first_did)))
        .await
        .unwrap();
    assert!(finished.accepted && finished.already_interrupted);
    assert!(finished.interrupted_at.is_none());
    assert!(!interrupted(core, first_did, "done").await);

    // Same logical request_id routes strictly to the explicit hosting runtime.
    submit_request(core, &served.runtime, &served.agent, "one", "same-id").await;
    submit_request(core, &second, &second_agent, "two", "same-id").await;
    interrupt_request(core, &user_cancel("same-id", Some(second_did)))
        .await
        .unwrap();
    assert!(!interrupted(core, first_did, "same-id").await);
    assert!(interrupted(core, second_did, "same-id").await);

    second.shutdown().await;
    served.shutdown().await;
}
