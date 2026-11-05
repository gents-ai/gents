use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(tag = "mode", rename_all = "snake_case"))]
pub enum NotificationIdentity {
    Event,
    Condition { key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxWriteOutcome {
    Created,
    Reused,
    Updated,
}

impl NotificationIdentity {
    pub fn source_id(
        &self,
        agent: &str,
        requester: &str,
        behavior: &str,
        event_id: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            !agent.trim().is_empty(),
            "notification requires node identity"
        );
        anyhow::ensure!(
            !requester.trim().is_empty(),
            "notification requires requester identity"
        );
        anyhow::ensure!(
            !behavior.trim().is_empty(),
            "notification requires agent identity"
        );
        let (mode, value) = match self {
            Self::Event => ("event", event_id),
            Self::Condition { key } => ("condition", key.as_str()),
        };
        anyhow::ensure!(
            !value.trim().is_empty(),
            "notification {mode} identity must not be empty"
        );
        Ok(serde_json::to_string(&(
            mode, agent, requester, behavior, value,
        ))?)
    }

    pub fn write_outcome(&self, open_exists: bool, same_content: bool) -> MailboxWriteOutcome {
        if !open_exists {
            MailboxWriteOutcome::Created
        } else if matches!(self, Self::Condition { .. }) && !same_content {
            MailboxWriteOutcome::Updated
        } else {
            MailboxWriteOutcome::Reused
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MailboxNotificationPolicy {
    pub identity: NotificationIdentity,
    pub kind: MailboxKind,
    pub action: MailboxAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub expected_collection: Option<String>,
}

impl Default for MailboxNotificationPolicy {
    fn default() -> Self {
        Self {
            identity: NotificationIdentity::Event,
            kind: MailboxKind::Flag,
            action: MailboxAction::Ack,
            expected_collection: None,
        }
    }
}

impl MailboxNotificationPolicy {
    pub fn validate(&self) -> Result<()> {
        self.identity
            .source_id("validation", "validation", "validation", "validation")?;
        match self.action {
            MailboxAction::WriteDocument => {
                let collection = self
                    .expected_collection
                    .as_deref()
                    .context("write_document notification requires expected_collection")?;
                validate_collection_identifier(collection)?;
            }
            _ => anyhow::ensure!(
                self.expected_collection.is_none(),
                "only write_document may set expected_collection"
            ),
        }
        Ok(())
    }
}

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
    access.transact_write("mailbox.update_condition", |txn| {
        Box::pin(async move {
            if let Some(source) = source { source.revalidate(txn).await?; }
            let response = txn.execute(&format!("{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {MAILBOX_FIELDS} }} }}", escape_graphql_string(&existing.doc_id))).await?;
            let values = gents_protocol::graphql::graphql_rows_from_response(&response, MAILBOX_COLLECTION);
            anyhow::ensure!(values.len() == 1, "condition notification disappeared");
            let item: MailboxItem = serde_json::from_value(values[0].clone())?;
            validate_existing_notification(&item, context, args)?;
            anyhow::ensure!(item.parsed_status() == Some(MailboxStatus::Open), "condition notification became terminal; retry to create a fresh occurrence");
            let same = item.title == args.title && item.summary == args.summary && item.payload == args.payload;
            let outcome = identity.write_outcome(true, same);
            if outcome == MailboxWriteOutcome::Reused {
                return Ok(MailboxWriteReceipt { outcome, item });
            }
            let response = txn.execute(&format!(
                "mutation {{ update_MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}, status: {{_eq: \"open\"}}}}, input: {{title: \"{}\", {} {} updated_at: \"{}\"}}) {{ {MAILBOX_FIELDS} }} }}",
                escape_graphql_string(&item.doc_id), escape_graphql_string(&args.title),
                optional_string_field("summary", args.summary.as_deref()),
                optional_string_field("payload", args.payload.as_deref()),
                escape_graphql_string(&Utc::now().to_rfc3339()),
            )).await?;
            let values = gents_protocol::graphql::graphql_rows_from_response(&response, "update_MailboxItem");
            anyhow::ensure!(values.len() == 1, "condition notification update did not commit exactly one open row");
            Ok(MailboxWriteReceipt { outcome, item: serde_json::from_value(values[0].clone())? })
        })
    }).await
}
