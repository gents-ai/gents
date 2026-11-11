use anyhow::{bail, Context, Result};

use crate::documents::{WriteToolDecl, WriteToolField};
use crate::session::mailbox::MailboxNotificationPolicy;
use gents_protocol::row::MAILBOX_COLLECTION;

pub const FILE_MAILBOX_ITEM_TOOL_NAME: &str = "file_mailbox_item";

fn canonical_field(name: &str, required: bool) -> WriteToolField {
    WriteToolField {
        name: name.to_string(),
        required,
        fill: None,
    }
}

pub fn canonical_mailbox_write_decl() -> WriteToolDecl {
    WriteToolDecl {
        notification: Some(MailboxNotificationPolicy::default()),
        tool_name: FILE_MAILBOX_ITEM_TOOL_NAME.to_string(),
        collection: MAILBOX_COLLECTION.to_string(),
        description: "Publish notification content using the configured identity and handling policy; runtime stamps ownership and provenance.".to_string(),
        fields: [
            ("title", true),
            ("summary", false),
            ("payload", false),
        ]
        .into_iter()
        .map(|(name, required)| canonical_field(name, required))
        .collect(),
        output_obligation: None,
    }
}

pub fn validate_mailbox_write_decl(decl: &WriteToolDecl) -> Result<()> {
    if decl.collection != MAILBOX_COLLECTION {
        anyhow::ensure!(
            decl.notification.is_none(),
            "notification policy is only valid for MailboxItem"
        );
        return Ok(());
    }
    decl.notification
        .as_ref()
        .context("MailboxItem requires an explicit notification policy")?
        .validate()?;
    let mut canonical = canonical_mailbox_write_decl();
    canonical.notification = decl.notification.clone();
    if decl != &canonical {
        bail!(
            "MailboxItem may only be targeted by the canonical `{FILE_MAILBOX_ITEM_TOOL_NAME}` declaration"
        );
    }
    Ok(())
}
