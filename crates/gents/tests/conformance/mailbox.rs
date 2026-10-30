use std::sync::Arc;

use gents::defra_node::EmbeddedNode;
use gents::session::mailbox::{
    dismiss_mailbox_item, load_mailbox_item, stamp_create, FileMailboxItemArgs, MailboxAction,
    MailboxKind, MailboxSourceKind, MailboxStampContext, MailboxStatus,
};

use crate::lean_vocab_test::{
    assert_state_machine_contract_is_complete, lean_state_machine_contract, lean_vocabulary_values,
};

#[test]
fn rust_mailbox_vocabularies_and_machine_match_lean_contract() {
    assert_eq!(
        lean_vocabulary_values("MailboxStatus"),
        MailboxStatus::ALL.map(MailboxStatus::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("MailboxKind"),
        MailboxKind::ALL.map(MailboxKind::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("MailboxHandling"),
        MailboxAction::ALL.map(MailboxAction::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("MailboxSourceKind"),
        MailboxSourceKind::ALL.map(MailboxSourceKind::as_str)
    );
    assert_state_machine_contract_is_complete("Mailbox");
}

#[tokio::test]
async fn rust_mailbox_dismiss_owner_drives_only_lean_legal_transitions() {
    let machine = lean_state_machine_contract("Mailbox");
    assert_eq!(
        machine.nonterminal_states,
        vec!["open".to_string()],
        "Lean Mailbox machine must keep exactly one nonterminal state"
    );
    for status in MailboxStatus::ALL {
        let lean_terminal = machine
            .terminal_states
            .iter()
            .any(|name| name == status.as_str());
        assert_eq!(
            lean_terminal,
            status.is_terminal(),
            "Rust MailboxStatus::{status:?} terminality disagrees with the Lean machine"
        );
    }
    assert!(
        machine
            .legal_transitions
            .iter()
            .all(|pair| pair.from == "open"),
        "every legal Mailbox transition must leave the single open state"
    );

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(gents::NodeIdentity::did(&identity))
            .build()
            .await
            .expect("embedded node"),
    );
    gents::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");

    let item = stamp_create(
        &node,
        &MailboxStampContext {
            requester_did: "did:test:owner".to_string(),
            node_did: "did:test:agent".to_string(),
            agent_id: "operator".to_string(),
            session_id: Some("session-1".to_string()),
        },
        FileMailboxItemArgs {
            kind: MailboxKind::Ask,
            action: MailboxAction::Ack,
            title: "Conformance item".to_string(),
            summary: None,
            payload: None,
            source_kind: MailboxSourceKind::Session,
            source_id: "wait-conformance".to_string(),
            session_id: None,
            request_id: None,
            cause_doc_id: None,
            expected_collection: None,
            parent_item_id: None,
            deadline_at: None,
        },
    )
    .await
    .expect("stamped open mailbox item");
    assert_eq!(item.status, "open");

    assert!(
        dismiss_mailbox_item(&node, &item.doc_id, "did:test:other")
            .await
            .is_err(),
        "non-owner dismissal must be refused"
    );
    assert_eq!(
        load_mailbox_item(&node, &item.doc_id)
            .await
            .expect("load after refused dismissal")
            .expect("stamped item still present")
            .status,
        "open",
        "refused dismissal must leave the item open"
    );

    let dismissed = dismiss_mailbox_item(&node, &item.doc_id, "did:test:owner")
        .await
        .expect("owner dismissal");
    assert_eq!(dismissed.status, "dismissed");
    assert!(
        dismissed.resolved_at.is_some(),
        "the open -> dismissed owner transition must stamp resolved_at"
    );

    let repeated = dismiss_mailbox_item(&node, &item.doc_id, "did:test:owner")
        .await
        .expect("re-dismiss of a terminal item is an idempotent no-op");
    assert_eq!(repeated.status, "dismissed");
    let resolved_instant = |raw: Option<&str>| {
        chrono::DateTime::parse_from_rfc3339(raw.expect("resolution timestamp"))
            .expect("valid resolution timestamp")
    };
    assert_eq!(
        resolved_instant(repeated.resolved_at.as_deref()),
        resolved_instant(dismissed.resolved_at.as_deref())
    );
}
