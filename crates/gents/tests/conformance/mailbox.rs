use gents::defra_node::EmbeddedNode;
use gents::mailbox::{
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

/// The Lean Mailbox machine carries a terminal/nonterminal partition that the
/// vocabulary check above never reads. The Rust owners gate every transition on
/// `MailboxStatus::is_terminal` and the requester principal, so this test pins
/// that partition to the Rust enum and then drives the real dismiss owner
/// through the machine's one legal dismissal transition and its terminal no-op.
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

    let node = EmbeddedNode::builder()
        .build()
        .await
        .expect("embedded node");
    gents::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");

    let item = stamp_create(
        &node,
        &MailboxStampContext {
            requester_did: "did:test:owner".to_string(),
            agent_did: "did:test:agent".to_string(),
            behavior_id: "operator".to_string(),
            session_id: Some("session-1".to_string()),
        },
        FileMailboxItemArgs {
            kind: MailboxKind::Ask,
            action: MailboxAction::Ack,
            title: "Conformance item".to_string(),
            summary: None,
            payload: None,
            source_kind: MailboxSourceKind::Graph,
            source_id: "wait-conformance".to_string(),
            session_id: None,
            request_id: None,
            graph_run_id: None,
            cause_doc_id: None,
            expected_collection: None,
            parent_item_id: None,
            deadline_at: None,
        },
    )
    .await
    .expect("stamped open mailbox item");
    assert_eq!(item.status, "open");

    // Lean applyResolution? refuses dismiss unless the principal is the
    // requester: the Rust owner must refuse without touching the open row.
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

    // Terminal states admit no transition in the Lean machine: re-dismiss is a
    // no-op that must not move the row again.
    let repeated = dismiss_mailbox_item(&node, &item.doc_id, "did:test:owner")
        .await
        .expect("re-dismiss of a terminal item is an idempotent no-op");
    assert_eq!(repeated.status, "dismissed");
    assert_eq!(repeated.resolved_at, dismissed.resolved_at);
}
