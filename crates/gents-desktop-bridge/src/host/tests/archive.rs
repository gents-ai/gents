use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::super::{approvals::AttachmentApprovals, protocol, ClientHost};

async fn archive(host: &ClientHost, archived: bool, revision: &str) -> Value {
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name": "desktop_session_archive", "args": {"request": {
            "nodeDid": "runtime", "sessionId": "shared-archive", "archived": archived,
            "expectedRevision": revision
        }}
    }))
    .unwrap();
    assert!(!command.is_cancellable());
    host.execute(
        command,
        &AttachmentApprovals::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

fn check_receipt(
    contract: &Value,
    receipt: Value,
    before_revision: u64,
    before_archived: bool,
    expected_revision: u64,
    archived: bool,
) -> u64 {
    let case = contract["session_document_cases"]["archive"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| {
            case["authorized"] == true
                && case["before"]["archive_revision"] == before_revision
                && !case["before"]["archived_at"].is_null() == before_archived
                && case["expected_revision"] == expected_revision
                && case["archived"] == archived
        })
        .expect("archive operation is covered by the executed model");
    assert_eq!(receipt["outcome"], case["decision"]);
    let revision = case["after"]["archive_revision"].as_u64().unwrap();
    assert_eq!(receipt["revision"], revision.to_string());
    assert_eq!(
        receipt["archivedAt"].is_null(),
        case["after"]["archived_at"].is_null()
    );
    revision
}

#[tokio::test]
async fn archive_receipts_and_shared_window_state_survive_client_reopen() {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    let contract: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let (core, home) = crate::tests::support::boot_core().await;
    crate::tests::support::create_session(&core, "runtime", "shared-archive", None).await;
    core.refresh_store().await.unwrap();
    let before = core
        .store()
        .snapshot()
        .sessions
        .iter()
        .find(|session| session.session_id == "shared-archive")
        .unwrap()
        .clone();
    let first = ClientHost::new(Arc::clone(&core));
    let second = ClientHost::new(Arc::clone(&core));
    check_receipt(
        &contract,
        archive(&first, true, "0").await,
        0,
        false,
        0,
        true,
    );
    check_receipt(
        &contract,
        archive(&second, false, "0").await,
        1,
        true,
        0,
        false,
    );
    check_receipt(
        &contract,
        archive(&second, true, "0").await,
        1,
        true,
        0,
        true,
    );
    let restored_revision = check_receipt(
        &contract,
        archive(&second, false, "1").await,
        1,
        true,
        1,
        false,
    );
    check_receipt(
        &contract,
        archive(&first, true, "0").await,
        restored_revision,
        false,
        0,
        true,
    );
    let invalid: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_session_archive", "args":{"request":{
            "nodeDid":"runtime", "sessionId":"shared-archive", "archived":true,
            "expectedRevision":"02"
        }}
    }))
    .unwrap();
    assert!(second
        .execute(
            invalid,
            &AttachmentApprovals::default(),
            CancellationToken::new()
        )
        .await
        .is_err());
    let history: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_session_history", "args":{"request":{
            "nodeDid":"runtime", "visibility":"active", "query":null, "cursor":null, "limit":30
        }}
    }))
    .unwrap();
    assert!(history.is_cancellable());
    let mut expected = before;
    expected.archive_revision = restored_revision;
    assert_eq!(
        core.store()
            .snapshot()
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
    assert_eq!(
        reopened
            .store()
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.session_id == "shared-archive"),
        Some(&expected)
    );
    Arc::new(reopened).shutdown().await.unwrap();
}
