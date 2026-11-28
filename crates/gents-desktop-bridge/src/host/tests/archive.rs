use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::super::{protocol, ClientHost};

async fn archive(
    host: &ClientHost,
    node: &str,
    session_id: &str,
    archived: bool,
    revision: &str,
) -> Value {
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name": "desktop_session_archive", "args": {"request": {
            "nodeDid": node, "sessionId": session_id, "archived": archived,
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
    use crate::client_runtime::ClientRuntime;
    use gents_store::client_journal::paths::DesktopPaths;

    // The sessions live on the served runtime; the desktop holds no replica,
    // so a reopened client reads every archive state back from its runtime.
    let home = tempfile::tempdir().unwrap();
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            DesktopPaths::from_root(home.path()),
            crate::client_test_support::file_journal_options(),
        )
        .await
        .unwrap(),
    );
    core.stop_observer_for_test().await;
    let (runtime, agent) = crate::tests::support::served_runtime_for(&core).await;
    let node = runtime.node_did().to_owned();
    let first = ClientHost::new(Arc::clone(&core));
    let second = ClientHost::new(Arc::clone(&core));
    let mut expected_sessions = Vec::new();

    for (case_index, case) in gents_lean_vocab::families::contracts_archive_receipts::cases()
        .into_iter()
        .enumerate()
    {
        let session_id = format!("shared-archive-{case_index}");
        let request_id = format!("opening-{case_index}");
        crate::tests::support::submit_request(&core, &runtime, &agent, &session_id, &request_id)
            .await;
        core.refresh_node(&node).await.unwrap();
        let before = core
            .stores()
            .snapshot(&node)
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .unwrap()
            .clone();
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
                &session_id,
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
        let mut expected = before;
        expected.archive_revision = restored_revision;
        assert_eq!(
            core.stores()
                .snapshot(&node)
                .sessions
                .iter()
                .find(|session| session.session_id == session_id),
            Some(&expected)
        );
        expected_sessions.push((session_id, expected));
    }

    let invalid: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_session_archive", "args":{"request":{
            "nodeDid":node, "sessionId":"shared-archive-0", "archived":true,
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

    drop(first);
    drop(second);
    core.shutdown().await.unwrap();
    drop(core);
    let reopened = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(home.path()),
        crate::client_test_support::file_journal_options(),
    )
    .await
    .unwrap();
    crate::client_test_support::connect_runtime(&runtime, &reopened).await;
    reopened.refresh_node(&node).await.unwrap();
    let reopened_snapshot = reopened.stores().snapshot(&node);
    for (session_id, expected) in &expected_sessions {
        assert_eq!(
            reopened_snapshot
                .sessions
                .iter()
                .find(|session| session.session_id == *session_id),
            Some(expected)
        );
    }
    Arc::new(reopened).shutdown().await.unwrap();
    runtime.shutdown().await;
}
