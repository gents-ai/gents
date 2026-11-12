use super::*;
use gents_session::request::lifecycle::RequestTerminalOutcome;

#[tokio::test]
async fn successor_acknowledges_input_left_by_a_failed_active_wake() {
    use gents_session::request::watcher::{DefraWatcher, Watcher};
    use gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified;
    let (selected, successor_child) = crate::toolcalls_tests::lifecycle::completion_owner_conformance::selected_background_wake_with_successor(true).await;
    let admission = selected.admission;
    let node = admission.node.clone();
    let did = admission.node_did.clone();
    let agent_signer = admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    let first_request = selected.wake;
    let agent_did = first_request.agent_did.clone();
    let first_id = first_request.request_id.clone();
    let session_id = first_request.session_id.clone();
    let mut first_lifecycle =
        gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            node.clone(),
            first_request,
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
    first_lifecycle.bind_managed_agent(&agent_signer).unwrap();
    assert_eq!(
        first_lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        &agent_signer,
        std::time::Duration::ZERO,
    )
    .unwrap();
    first_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    writer
        .publish_authored_message(
            &first_lifecycle,
            "unrelated-foreground-input",
            &gents_protocol::message::Message::user("unrelated foreground input"),
        )
        .await
        .unwrap();
    complete_child_managed_verified(
        &node,
        &successor_child.unwrap(),
        "second notification",
        &agent_signer,
        admission.runtime.clone(),
    )
    .await;
    assert_eq!(
        gents_toolcalls::background::completion::settle_running_session_message_rows_with_access(
            &node,
            &admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
            &did
        )
        .await
        .unwrap(),
        1
    );
    first_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some("injected provider failure"),
        )
        .await
        .unwrap();
    let access = admission.tool.access().unwrap();
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{ session_id: {{ _eq: \"{}\" }} }}) {{ retry_key }} }}",
            escape_graphql_string(&session_id)
        ))
        .await
        .unwrap();
    let mut retry_keys = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["retry_key"].as_str())
        .filter(|key| key.starts_with("background-completion:"))
        .collect::<Vec<_>>();
    retry_keys.sort_unstable();
    assert_eq!(retry_keys.len(), 2);
    assert!(retry_keys[0].ends_with(":00000000000000000000"));
    assert!(retry_keys[1].ends_with(":00000000000000000001"));
    assert_eq!(
        retry_keys[0].rsplit_once(':').unwrap().0,
        retry_keys[1].rsplit_once(':').unwrap().0,
        "unrelated transcript writes must not advance the queue-local generation"
    );
    let agent = gents_documents::load_agent(&node, &did, &agent_did)
        .await
        .unwrap()
        .expect("fixture Agent binding");
    let agent_identity = gents_store::identity::resolve_agent_identity(
        admission.runtime.as_ref(),
        &gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent.agent_did,
            creation_nonce: agent.creation_nonce,
        },
    )
    .unwrap();
    let mut watcher = DefraWatcher::for_agents(node.clone(), &did, vec![agent_identity])
        .expect("request watcher");
    let second_request = watcher.next_request().await.unwrap().unwrap();
    assert_ne!(second_request.request_id, first_id);
    assert_eq!(second_request.agent_did, agent_did);
    let mut second_lifecycle =
        gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            node.clone(),
            second_request,
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
    second_lifecycle.bind_managed_agent(&agent_signer).unwrap();
    assert_eq!(
        second_lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    second_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    second_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    let diagnostics =
        gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
            &access, &did,
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.pending_notifications, 0);
    assert_eq!(diagnostics.acknowledged_notifications, 2);
    assert_eq!(diagnostics.stranded_notifications, 0);
    let first_epoch = diagnostics
        .epochs
        .iter()
        .find(|epoch| epoch.root_request_id == first_id)
        .unwrap();
    assert_eq!(first_epoch.state, "acknowledged_by_successor");
    let timeline =
        gents_observe::observe::run_timeline_fetch::load_run_timeline(&access, &first_id)
            .await
            .unwrap();
    assert_eq!(timeline.background_completions.len(), 2);
    assert!(timeline.background_completion_diagnostics_error.is_none());
    assert!(timeline.child_request_ids.is_empty());
    drop(watcher);
    drop(first_lifecycle);
    drop(second_lifecycle);
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
