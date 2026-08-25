use anyhow::{bail, Context, Result};
use gents_desktop_core::client::ClientCore;

use super::super::types::MailboxItemView;

pub fn list_mailbox(core: &ClientCore) -> Vec<MailboxItemView> {
    let requester = core.principal().did();
    core.store()
        .snapshot()
        .mailbox_items
        .iter()
        .filter(|row| row.requester_did == requester && row.status == "open")
        .map(MailboxItemView::from)
        .collect()
}

pub fn start_mailbox_request(core: &ClientCore, item_id: &str) -> Result<MailboxItemView> {
    let requester = core.principal().did();
    let snapshot = core.store().snapshot();
    let row = snapshot
        .mailbox_items
        .iter()
        .find(|row| row.doc_id == item_id)
        .context("MailboxItem not found")?;
    if row.requester_did != requester {
        bail!("only requester_did may act on a MailboxItem");
    }
    if row.status != "open" {
        bail!("MailboxItem is no longer open");
    }
    if !matches!(row.action.as_str(), "start_request" | "write_document") {
        bail!("MailboxItem action does not open a compose surface");
    }
    Ok(MailboxItemView::from(row))
}

pub async fn dismiss_mailbox(core: &ClientCore, item_id: &str) -> Result<()> {
    core.dismiss_mailbox_item(item_id).await.map(|_| ())
}
