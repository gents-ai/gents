use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{protocol, ClientHost};

#[tokio::test]
async fn runtime_location_accepts_a_saved_runtime_without_connecting() {
    use crate::client_runtime::ClientRuntime;
    use gents_store::client_journal::paths::DesktopPaths;

    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path());
    let connection = gents_protocol::runtime_connection::RuntimeConnection {
        node_did: "did:paired".into(),
        label: "Remote runtime".into(),
        graphql: "http://127.0.0.1:1/api/v0/graphql".into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    std::fs::write(
        paths.runtime_directory_path(),
        json!({"connections": [connection]}).to_string(),
    )
    .unwrap();
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths,
            crate::client_test_support::file_journal_options(),
        )
        .await
        .unwrap(),
    );
    let host = ClientHost::new(core.clone());
    assert_eq!(
        host.execute(
            protocol::ClientCommand::RuntimeIsLocal {
                node_did: "did:paired".into()
            },
            &crate::host::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap(),
        false
    );
    assert!(host.require_configured_runtime("did:paired").await.is_ok());
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn private_snapshot_and_paired_model_settings_use_the_authenticated_client_endpoint() {
    let served = crate::tests::support::served_desktop().await;
    let core = served.core.clone();
    core.stop_observer_for_test().await;
    let requester = core.node_identity().did().to_owned();
    let node = served.node_did().to_owned();

    // Model settings are read as the User over the paired runtime's
    // authenticated GraphQL; the served runtime refuses any other actor.
    let host = ClientHost::new(core.clone());
    let settings = host
        .execute(
            protocol::ClientCommand::InferenceSetupState {
                node_did: node.clone(),
            },
            &crate::host::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(settings["nodeDid"], node.as_str());
    assert!(settings["agentConfigs"]
        .as_array()
        .is_some_and(|agents| !agents.is_empty()));
    host.require_configured_runtime(&node).await.unwrap();

    let agent = served.runtime.agent_access(&served.agent).await;
    let file = |id: &str, recipient: &str, kind: &str, action: &str, payload: Option<String>| {
        let context = gents_session::session::mailbox::MailboxStampContext {
            requester_did: recipient.to_owned(),
            node_did: node.clone(),
            agent_did: served.agent.agent_did.clone(),
            session_id: Some("chat".into()),
        };
        let args: gents_session::session::mailbox::FileMailboxItemArgs = serde_json::from_value(json!({
            "kind": kind, "action": action, "title": "Choose work", "payload": payload,
            "source_kind": "agent", "source_id": id, "session_id": "chat", "request_id": "request",
        }))
        .unwrap();
        let (runtime, agent) = (served.runtime.node().clone(), agent.clone());
        async move {
            gents_session::session::mailbox::stamp_create_with_access(
                &runtime,
                &agent,
                &context,
                args,
                gents_session::session::mailbox::MAILBOX_CLOSE_COLLECTIONS,
            )
            .await
            .unwrap()
        }
    };
    let question =
        json!({"version":3,"prompt":"What should happen next?","options":[],"allow_free_text":true})
            .to_string();
    file(
        "current",
        &requester,
        "ask",
        "start_request",
        Some(question.clone()),
    )
    .await;
    file(
        "other-requester",
        "did:key:z6MkwVDfCg9LbbY6xjH3EZk8YSFQZujV5Y4y1ZWeER9tDiN3",
        "ask",
        "start_request",
        Some(question),
    )
    .await;
    let closed = file("closed", &requester, "flag", "ack", None).await;
    gents_session::operations::mailbox::dismiss(
        &core.operator_access(&node).unwrap(),
        &closed.doc_id,
    )
    .await
    .unwrap();
    let user = ::identity::Did::new(&requester).unwrap();
    gents_store::identity::GrantSet::OperatorUser
        .grant(&served.runtime.runtime_access(), &user)
        .await
        .unwrap();
    gents_store::identity::GrantSet::HouseholdOwner
        .revoke(&served.runtime.runtime_access(), &user)
        .await
        .unwrap();
    core.refresh_node(&node).await.unwrap();
    let result = host
        .execute(
            protocol::ClientCommand::Snapshot {},
            &crate::host::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let deployments = result["client"]["deployments"].as_array().unwrap();
    let [deployment] = deployments.as_slice() else {
        panic!("one paired runtime: {result}")
    };
    let items = deployment["mailboxItems"].as_array().unwrap();
    let [own] = items.as_slice() else {
        panic!("expected only the current requester's open question: {items:?}")
    };
    assert_eq!(own["sourceId"], "current");
    assert_eq!(own["requesterDid"], requester);
    assert!(own["questionHash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(result["bootstrap"]["desktopHome"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(!host.policy.snapshot_grants.config_read);
    assert!(!host.policy.snapshot_grants.runtime_admin);
    drop(host);
    drop(core);
    served.shutdown().await;
}
