use gents::mailbox::{MailboxAction, MailboxKind, MailboxSourceKind, MailboxStatus};

use crate::lean_vocab_test::{assert_state_machine_contract_is_complete, lean_vocabulary_values};

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
