use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::documents::{WriteToolDecl, WriteToolField};
use crate::store::graphql::validate_collection_identifier;
use gents_protocol::row::MAILBOX_COLLECTION;

pub const FILE_MAILBOX_ITEM_TOOL_NAME: &str = "file_mailbox_item";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum MailboxKind {
    Ask,
    Gate,
    Finished,
    Failed,
    Flag,
}

impl MailboxKind {
    pub const ALL: [Self; 5] = [
        Self::Ask,
        Self::Gate,
        Self::Finished,
        Self::Failed,
        Self::Flag,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Gate => "gate",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Flag => "flag",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum MailboxAction {
    Ack,
    StartRequest,
    WriteDocument,
}

impl MailboxAction {
    pub const ALL: [Self; 3] = [Self::Ack, Self::StartRequest, Self::WriteDocument];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::StartRequest => "start_request",
            Self::WriteDocument => "write_document",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
