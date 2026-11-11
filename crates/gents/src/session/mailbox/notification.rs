use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxContentArgs {
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    #[serde(default)]
    pub question: Option<gents_protocol::mailbox_question::MailboxQuestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailboxWriteReceipt {
    pub outcome: MailboxWriteOutcome,
    pub item: MailboxItem,
}

fn validate_existing_notification(
    item: &MailboxItem,
    context: &MailboxStampContext,
    args: &FileMailboxItemArgs,
) -> Result<()> {
    anyhow::ensure!(
        item.requester_did == context.requester_did
            && item.node_did == context.node_did
            && item.target_node_did == context.node_did
            && item.target_agent_did == context.agent_did
            && item.source_id == args.source_id
            && item.source_kind == args.source_kind.as_str()
            && item.kind == args.kind.as_str(),
        "notification identity mismatch"
    );
    anyhow::ensure!(
        item.action == args.action.as_str() && item.expected_collection == args.expected_collection,
        "open notification has a different handling policy; resolve it before changing policy"
    );
    Ok(())
}

#[cfg(test)]
pub(super) async fn reuse_or_update(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    context: &MailboxStampContext,
    args: &FileMailboxItemArgs,
    identity: &NotificationIdentity,
    existing: &MailboxItem,
) -> Result<MailboxWriteReceipt> {
    reuse_or_update_with_source(node, access, context, args, identity, existing, None).await
}

pub(super) async fn reuse_or_update_with_source(
    _node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    context: &MailboxStampContext,
    args: &FileMailboxItemArgs,
    identity: &NotificationIdentity,
    existing: &MailboxItem,
    source: Option<&crate::documents::DatastoreToolSource>,
) -> Result<MailboxWriteReceipt> {
    validate_existing_notification(existing, context, args)?;
    if source.is_none() && matches!(identity, NotificationIdentity::Event) {
        return Ok(MailboxWriteReceipt {
            outcome: MailboxWriteOutcome::Reused,
            item: existing.clone(),
        });
    }
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(context.agent_did.as_str()),
        "mailbox actor mismatch"
    );
    access
        .transact_write("mailbox.update_condition", |txn| {
            Box::pin(async move {
                if let Some(source) = source {
                    source.revalidate(txn).await?;
                }
                let response = {
                    use gents_protocol::document_query::{
                        fields::mailbox_item as fields, Filter, Query, Selection,
                    };
                    txn.run(
                        &Query::select(Selection::owned(MAILBOX_FIELDS))
                            .filter(Filter::eq(fields::DOC_ID, &existing.doc_id))
                            .render(),
                    )
                    .await?
                };
                let values = gents_protocol::graphql::graphql_rows_from_response(
                    &response,
                    MAILBOX_COLLECTION,
                );
                anyhow::ensure!(values.len() == 1, "condition notification disappeared");
                let item: MailboxItem = serde_json::from_value(values[0].clone())?;
                validate_existing_notification(&item, context, args)?;
                anyhow::ensure!(
                    item.parsed_status() == Some(MailboxStatus::Open),
                    "condition notification became terminal; retry to create a fresh occurrence"
                );
                let same = item.title == args.title
                    && item.summary == args.summary
                    && item.payload == args.payload;
                let outcome = identity.write_outcome(true, same);
                if outcome == MailboxWriteOutcome::Reused {
                    return Ok(MailboxWriteReceipt { outcome, item });
                }
                let response = {
                    use gents_protocol::document_query::{
                        fields::mailbox_item as fields, Filter, Input, Selection, Update,
                    };
                    let summary = args
                        .summary
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let payload = args
                        .payload
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let rendered = Update::matching(
                        Filter::eq(fields::DOC_ID, &item.doc_id)
                            .and(Filter::eq(fields::STATUS, "open")),
                        Input::new()
                            .set(fields::TITLE, &args.title)
                            .set_optional(fields::SUMMARY, summary)
                            .set_optional(fields::PAYLOAD, payload)
                            .set(fields::UPDATED_AT, Utc::now().to_rfc3339()),
                    )
                    .returning(Selection::owned(MAILBOX_FIELDS))
                    .render();
                    txn.run(&rendered).await?
                };
                let values = gents_protocol::graphql::graphql_rows_from_response(
                    &response,
                    "update_MailboxItem",
                );
                anyhow::ensure!(
                    values.len() == 1,
                    "condition notification update did not commit exactly one open row"
                );
                Ok(MailboxWriteReceipt {
                    outcome,
                    item: serde_json::from_value(values[0].clone())?,
                })
            })
        })
        .await
}
