use super::*;
use gents_protocol::mailbox_question::question_requires_reply_receipt;

pub async fn dismiss_mailbox_item(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    principal_did: &str,
) -> Result<MailboxItem> {
    crate::ConfigAccess::transact_local_write(node, None, "mailbox.dismiss", |txn| {
        Box::pin(async move {
            let response = txn.execute_local_response(&format!(
                r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {MAILBOX_FIELDS} }} }}"#,
                escape_graphql_string(doc_id),
            )).await?;
            let item = rows::<MailboxItem>(&response, MAILBOX_COLLECTION)?
                .into_iter().next().context("MailboxItem not found")?;
            if item.requester_did != principal_did.trim() {
                bail!("only requester_did may dismiss a MailboxItem");
            }
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
            let now = escape_graphql_string(&Utc::now().to_rfc3339());
            let response = txn.execute_local_response(&format!(
                r#"mutation {{ update_MailboxItem(
                    filter: {{ _docID: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }},
                    input: {{ status: "dismissed", updated_at: "{now}", resolved_at: "{now}", resolved_doc_id: null }}
                ) {{ {MAILBOX_FIELDS} }} }}"#,
                escape_graphql_string(doc_id),
            )).await?;
            let document = single_mutation_document(&response, "update_MailboxItem")?
                .context("mailbox dismissal did not consume an open item")?;
            Ok(serde_json::from_value(document.clone())?)
        })
    }).await
}
