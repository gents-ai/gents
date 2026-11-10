use std::sync::Arc;

use gents::defra_node::Node as EmbeddedNode;
use gents::session::mailbox::{
    dismiss_mailbox_item_on, stamp_create_with_access, FileMailboxItemArgs, MailboxAction,
    MailboxKind, MailboxSourceKind, MailboxStampContext, MailboxStatus, MAILBOX_CLOSE_COLLECTIONS,
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
    let requester =
        gents::KeyIdentity::load_or_create(directory.path().join("requester.key"), None).unwrap();
    let other =
        gents::KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
    let agent_binding = gents::store::identity::provision_agent_identity(&identity).unwrap();
    let agent = gents::store::identity::resolve_agent_identity(&identity, &agent_binding).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(gents::NodeIdentity::did(&identity))
            .with_node_acp_enabled()
            .build()
            .await
            .expect("embedded node"),
    );
    gents::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");

    let admin = gents::ConfigAccess::local_as(
        node.clone(),
        gents::NodeIdentity::did(&identity).parse().unwrap(),
    );
    for actor in [agent.did(), gents::NodeIdentity::did(&other)] {
        for permission in [
            gents::store::config_client::NodePermission::DocumentRead,
            gents::store::config_client::NodePermission::DocumentUpdate,
            gents::store::config_client::NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let creator = gents::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let outsider = gents::ConfigAccess::local_as(
        node.clone(),
        gents::NodeIdentity::did(&other).parse().unwrap(),
    );
    let item = stamp_create_with_access(
        &node,
        &creator,
        &MailboxStampContext {
            requester_did: gents::NodeIdentity::did(&requester).to_string(),
            node_did: gents::NodeIdentity::did(&identity).to_string(),
            agent_did: agent.did().to_string(),
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
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
    .expect("stamped open mailbox item");
    assert_eq!(item.status, "open");

    assert!(dismiss_mailbox_item_on(&outsider, &item.doc_id)
        .await
        .is_err());
    let dismissed = dismiss_mailbox_item_on(&creator, &item.doc_id)
        .await
        .expect("native-authorized dismissal is independent of session requester provenance");
    assert_eq!(dismissed.status, "dismissed");
    assert!(
        dismissed.resolved_at.is_some(),
        "the open -> dismissed transition must stamp resolved_at"
    );

    let repeated = dismiss_mailbox_item_on(&creator, &item.doc_id)
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
