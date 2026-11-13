use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::super::{protocol, ClientHost};

async fn archive(host: &ClientHost, node: &str, archived: bool, revision: &str) -> Value {
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name": "desktop_session_archive", "args": {"request": {
            "nodeDid": node, "sessionId": "shared-archive", "archived": archived,
            "expectedRevision": revision
        }}
    }))
    .unwrap();
    assert!(!command.is_cancellable());
    host.execute(
        command,
        &crate::host::approvals::AttachmentApprovals::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn archive_receipts_and_shared_window_state_survive_client_reopen() {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    for case in gents_lean_vocab::families::contracts_archive_receipts::cases() {
        // The session lives on the served runtime; the desktop holds no replica,
        // so a reopened client reads the archive state back from its runtime.
        let home = tempfile::tempdir().unwrap();
        let core = Arc::new(
            ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(home.path()),
                ClientCoreOptions::local_only(),
            )
            .await
            .unwrap(),
        );
        core.stop_observer_for_test().await;
        let (runtime, agent) = crate::tests::support::served_runtime_for(
            &core,
            crate::tests::support::ServedRoute::CoHosted,
        )
        .await;
        let node = runtime.node_did().to_owned();
        crate::tests::support::submit_request(&core, &runtime, &agent, "shared-archive", "opening")
            .await;
        core.refresh_node(&node).await.unwrap();
        let before = core
            .stores()
            .snapshot(&node)
            .sessions
            .iter()
            .find(|session| session.session_id == "shared-archive")
            .unwrap()
            .clone();
        let first = ClientHost::new(Arc::clone(&core));
        let second = ClientHost::new(Arc::clone(&core));
        let mut receipts = Vec::new();
        for (index, step) in case.input.steps.iter().enumerate() {
            let host = if index == 0 || index + 1 == case.input.steps.len() {
                &first
            } else {
                &second
            };
            let receipt = archive(
                host,
                &node,
                step.archived,
                &step.expected_revision.to_string(),
            )
            .await;
            receipts.push(
                gents_lean_vocab::families::contracts_archive_receipts::ArchiveReceipt {
                    decision: receipt["outcome"].as_str().unwrap().to_owned(),
                    revision: receipt["revision"].as_str().unwrap().parse().unwrap(),
                    archived: !receipt["archivedAt"].is_null(),
                },
            );
        }
        let restored_revision = receipts.last().unwrap().revision;
        case.check(receipts);
        let invalid: protocol::ClientCommand = serde_json::from_value(json!({
            "name":"desktop_session_archive", "args":{"request":{
                "nodeDid":node, "sessionId":"shared-archive", "archived":true,
                "expectedRevision":"02"
            }}
        }))
        .unwrap();
        assert!(second
            .execute(
                invalid,
                &crate::host::approvals::AttachmentApprovals::default(),
                CancellationToken::new()
            )
            .await
            .is_err());
        let history: protocol::ClientCommand = serde_json::from_value(json!({
            "name":"desktop_session_history", "args":{"request":{
                "nodeDid":node, "visibility":"active", "query":null, "cursor":null, "limit":30
            }}
        }))
        .unwrap();
        assert!(history.is_cancellable());
        let mut expected = before;
        expected.archive_revision = restored_revision;
        assert_eq!(
            core.stores()
                .snapshot(&node)
                .sessions
                .iter()
                .find(|session| session.session_id == "shared-archive"),
            Some(&expected)
        );
        drop(first);
        drop(second);
        core.shutdown().await.unwrap();
        drop(core);
        let reopened = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(home.path()),
            ClientCoreOptions::local_only(),
        )
        .await
        .unwrap();
        runtime.connect(&reopened).await;
        reopened.refresh_node(&node).await.unwrap();
        assert_eq!(
            reopened
                .stores()
                .snapshot(&node)
                .sessions
                .iter()
                .find(|session| session.session_id == "shared-archive"),
            Some(&expected)
        );
        Arc::new(reopened).shutdown().await.unwrap();
        runtime.shutdown().await;
    }
}
