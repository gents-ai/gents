use super::*;
use gents_protocol::mailbox_question::question_requires_reply_receipt;

pub async fn dismiss_mailbox_item(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    principal_did: &str,
) -> Result<MailboxItem> {
    let access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(principal_did.to_owned())?);
    dismiss_mailbox_item_on(&access, doc_id).await
}

/// Dismiss through the authenticated access owner, preserving the caller's
/// document actor for both embedded and remote transactions.
pub async fn dismiss_mailbox_item_on(
    access: &crate::ConfigAccess,
    doc_id: &str,
) -> Result<MailboxItem> {
    anyhow::ensure!(
        access.actor_did().is_some(),
        "mailbox dismissal requires an authenticated actor"
    );
    access.transact_write("mailbox.dismiss", |txn| {
        Box::pin(async move {
            let response = txn.execute(&format!(
                r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {MAILBOX_FIELDS} }} }}"#,
                escape_graphql_string(doc_id),
            )).await?;
            let response = defra_node::QueryResponse::success(response.get("data").context("mailbox read omitted data")?.clone());
            let item = rows::<MailboxItem>(&response, MAILBOX_COLLECTION)?
                .into_iter().next().context("MailboxItem not found")?;
            if item.parsed_status().is_some_and(MailboxStatus::is_terminal) {
                return Ok(item);
            }
            if item.parsed_status() != Some(MailboxStatus::Open) {
                bail!("MailboxItem has unknown status {:?}", item.status);
            }
            if item.kind == MailboxKind::Ask.as_str()
                && item.parsed_action() == Some(MailboxAction::StartRequest)
                && item.payload.as_deref().is_some_and(question_requires_reply_receipt)
            {
                bail!("reply to this question through its original item; use an answer or explicit Skip when optional");
            }
            let now = Utc::now().to_rfc3339();
            let mutation = format!(
                r#"mutation($id: ID!, $input: MailboxItemMutationInputArg!) {{ update_MailboxItem(
                    filter: {{ _docID: {{ _eq: $id }}, status: {{ _eq: "open" }} }},
                    input: $input
                ) {{ {MAILBOX_FIELDS} }} }}"#,
            );
            let response = txn.execute_client_signed_update(
                &mutation,
                &serde_json::json!({"id": doc_id, "input": {"status": "dismissed", "updated_at": now, "resolved_at": now, "resolved_doc_id": null}}),
                gents_protocol::schemas::MAILBOX_ITEM,
            ).await?;
            let response = defra_node::QueryResponse::success(response.get("data").context("mailbox update omitted data")?.clone());
            let document = single_mutation_document(&response, "update_MailboxItem")?
                .context("mailbox dismissal did not consume an open item")?;
            Ok(serde_json::from_value(document.clone())?)
        })
    }).await
}
