use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::document_config::{WriteToolDecl, WriteToolField};
use crate::graphql::{
    escape_graphql_string, graphql_with_transaction_retry, rows, single_mutation_document,
    validate_collection_identifier, validate_graphql_name,
};
use crate::llm::tool::ToolDefinition;

mod dismiss;
mod domain;
pub use dismiss::dismiss_mailbox_item;
mod notification;
mod reply;
mod waiting;
pub(crate) use reply::claim_reply_in_txn;
pub use waiting::awaiting_question_ids;
pub(crate) use waiting::required_question_in_txn;
#[cfg(test)]
mod reply_tests;
pub use notification::{
    MailboxContentArgs, MailboxNotificationPolicy, MailboxWriteOutcome, MailboxWriteReceipt,
    NotificationIdentity,
};
#[cfg(test)]
mod notification_tests;

pub const MAILBOX_COLLECTION: &str = "MailboxItem";
pub const FILE_MAILBOX_ITEM_TOOL_NAME: &str = "file_mailbox_item";

pub const MAILBOX_FIELDS: &str = r#"
    _docID
    item_key
    requester_did
    node_did
    status
    kind
    action
    title
    summary
    payload
    source_kind
    source_id
    session_id
    request_id
    cause_doc_id
    target_node_did
    target_agent_id
    expected_collection
    parent_item_id
    deadline_at
    created_at
    updated_at
    resolved_at
    resolved_doc_id
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxStatus {
    Open,
    Acted,
    Dismissed,
    Expired,
}

impl MailboxStatus {
    pub const ALL: [Self; 4] = [Self::Open, Self::Acted, Self::Dismissed, Self::Expired];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acted => "acted",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }

    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Open)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxSourceKind {
    Session,
    Agent,
    Runtime,
    Tool,
}

impl MailboxSourceKind {
    pub const ALL: [Self; 4] = [Self::Session, Self::Agent, Self::Runtime, Self::Tool];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Agent => "agent",
            Self::Runtime => "runtime",
            Self::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MailboxItem {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub item_key: String,
    pub requester_did: String,
    pub node_did: String,
    pub status: String,
    pub kind: String,
    pub action: String,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub cause_doc_id: Option<String>,
    pub target_node_did: String,
    pub target_agent_id: String,
    #[serde(default)]
    pub expected_collection: Option<String>,
    #[serde(default)]
    pub parent_item_id: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved_doc_id: Option<String>,
}

impl MailboxItem {
    pub fn parsed_status(&self) -> Option<MailboxStatus> {
        MailboxStatus::parse(&self.status)
    }

    pub fn parsed_action(&self) -> Option<MailboxAction> {
        MailboxAction::parse(&self.action)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxStampContext {
    pub requester_did: String,
    pub node_did: String,
    pub agent_id: String,
    pub session_id: Option<String>,
}

impl MailboxStampContext {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("requester_did", self.requester_did.as_str()),
            ("node_did", self.node_did.as_str()),
            ("agent_id", self.agent_id.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("mailbox stamping requires non-empty {name}");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMailboxItemArgs {
    pub kind: MailboxKind,
    pub action: MailboxAction,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    pub source_kind: MailboxSourceKind,
    pub source_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub cause_doc_id: Option<String>,
    #[serde(default)]
    pub expected_collection: Option<String>,
    #[serde(default)]
    pub parent_item_id: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
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

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn optional_string_field(name: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#"{name}: "{}","#, escape_graphql_string(value)),
        None => format!("{name}: null,"),
    }
}

fn item_key_occurrence(item_key: &str) -> Option<u64> {
    item_key.rsplit_once(':')?.1.parse().ok()
}

async fn load_prefix_items(
    node: &EmbeddedNode,
    requester_did: &str,
    source_kind: MailboxSourceKind,
    source_id: &str,
    kind: MailboxKind,
) -> Result<Vec<MailboxItem>> {
    let query = format!(
        r#"{{
            MailboxItem(filter: {{
                requester_did: {{ _eq: "{}" }},
                source_kind: {{ _eq: "{}" }},
                source_id: {{ _eq: "{}" }},
                kind: {{ _eq: "{}" }}
            }}) {{ {MAILBOX_FIELDS} }}
        }}"#,
        escape_graphql_string(requester_did),
        source_kind.as_str(),
        escape_graphql_string(source_id),
        kind.as_str(),
    );
    let response = graphql_with_transaction_retry(node, &query, "load mailbox item prefix").await?;
    Ok(rows(&response, MAILBOX_COLLECTION)?)
}

pub async fn load_mailbox_item(node: &EmbeddedNode, doc_id: &str) -> Result<Option<MailboxItem>> {
    let query = format!(
        r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {MAILBOX_FIELDS} }} }}"#,
        escape_graphql_string(doc_id)
    );
    let response = graphql_with_transaction_retry(node, &query, "load mailbox item").await?;
    let mut found = rows::<MailboxItem>(&response, MAILBOX_COLLECTION)?;
    if found.len() > 1 {
        bail!("MailboxItem _docID lookup returned more than one row");
    }
    Ok(found.pop())
}

async fn load_mailbox_item_by_key(
    node: &EmbeddedNode,
    item_key: &str,
) -> Result<Option<MailboxItem>> {
    let query = format!(
        r#"{{ MailboxItem(filter: {{ item_key: {{ _eq: "{}" }} }}, limit: 2) {{ {MAILBOX_FIELDS} }} }}"#,
        escape_graphql_string(item_key)
    );
    let response = graphql_with_transaction_retry(node, &query, "load mailbox item by key").await?;
    let mut found = rows::<MailboxItem>(&response, MAILBOX_COLLECTION)?;
    if found.len() > 1 {
        bail!("MailboxItem item_key lookup returned more than one row");
    }
    Ok(found.pop())
}

fn validate_file_args(
    args: &mut FileMailboxItemArgs,
    context: &MailboxStampContext,
    _close_collections: &[MailboxCloseCollection],
) -> Result<()> {
    context.validate()?;
    args.title = args.title.trim().to_string();
    args.source_id = args.source_id.trim().to_string();
    args.summary = nonempty(args.summary.take());
    args.payload = nonempty(args.payload.take());
    args.session_id = nonempty(args.session_id.take()).or_else(|| context.session_id.clone());
    args.request_id = nonempty(args.request_id.take());
    args.cause_doc_id = nonempty(args.cause_doc_id.take());
    args.expected_collection = nonempty(args.expected_collection.take());
    args.parent_item_id = nonempty(args.parent_item_id.take());
    args.deadline_at = nonempty(args.deadline_at.take());
    if args.title.is_empty() || args.source_id.is_empty() {
        bail!("mailbox title and source_id must not be empty");
    }
    if let Some(deadline) = args.deadline_at.as_deref() {
        DateTime::parse_from_rfc3339(deadline).context("deadline_at must be RFC3339")?;
    }
    match args.action {
        MailboxAction::WriteDocument => {
            let expected = args
                .expected_collection
                .as_deref()
                .context("write_document requires expected_collection")?;
            validate_collection_identifier(expected)?;
        }
        MailboxAction::Ack | MailboxAction::StartRequest => {
            if args.expected_collection.is_some() {
                bail!("{} must not set expected_collection", args.action.as_str());
            }
        }
    }
    Ok(())
}

pub async fn stamp_create(
    node: &Arc<EmbeddedNode>,
    context: &MailboxStampContext,
    args: FileMailboxItemArgs,
) -> Result<MailboxItem> {
    stamp_create_with_close_collections(node, context, args, MAILBOX_CLOSE_COLLECTIONS).await
}

pub async fn stamp_create_with_close_collections(
    node: &Arc<EmbeddedNode>,
    context: &MailboxStampContext,
    args: FileMailboxItemArgs,
    close_collections: &[MailboxCloseCollection],
) -> Result<MailboxItem> {
    Ok(stamp_notification(
        node,
        context,
        args,
        &NotificationIdentity::Event,
        close_collections,
    )
    .await?
    .item)
}

async fn stamp_notification(
    node: &std::sync::Arc<EmbeddedNode>,
    context: &MailboxStampContext,
    mut args: FileMailboxItemArgs,
    identity: &NotificationIdentity,
    close_collections: &[MailboxCloseCollection],
) -> Result<MailboxWriteReceipt> {
    validate_close_collections(close_collections)?;
    validate_file_args(&mut args, context, close_collections)?;
    if let Some(collection) = args.expected_collection.as_deref() {
        domain::response_field(node, collection, close_collections)?;
    }
    let existing = load_prefix_items(
        node,
        &context.requester_did,
        args.source_kind,
        &args.source_id,
        args.kind,
    )
    .await?;
    let open = existing
        .iter()
        .filter(|item| item.parsed_status() == Some(MailboxStatus::Open))
        .collect::<Vec<_>>();
    match open.as_slice() {
        [] => {}
        [item] if item.requester_did == context.requester_did => {
            return notification::reuse_or_update(node, context, &args, identity, item).await;
        }
        [_] => bail!("mailbox open-row owner mismatch"),
        _ => bail!("mailbox invariant violation: more than one owner-matching open row"),
    }
    let occurrence = existing
        .iter()
        .filter_map(|item| item_key_occurrence(&item.item_key))
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .context("mailbox occurrence overflow")?;
    let item_key = format!(
        "{}:{}:{}:{occurrence}",
        args.source_kind.as_str(),
        args.source_id,
        args.kind.as_str()
    );
    let now = Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_MailboxItem(input: {{
                item_key: "{item_key}",
                requester_did: "{requester_did}",
                node_did: "{node_did}",
                status: "open",
                kind: "{kind}",
                action: "{action}",
                title: "{title}",
                {summary}
                {payload}
                source_kind: "{source_kind}",
                source_id: "{source_id}",
                {session_id}
                {request_id}
                {cause_doc_id}
                target_node_did: "{target_node_did}",
                target_agent_id: "{target_agent_id}",
                {expected_collection}
                {parent_item_id}
                {deadline_at}
                created_at: "{now}",
                updated_at: "{now}",
                resolved_at: null,
                resolved_doc_id: null
            }}) {{ {MAILBOX_FIELDS} }}
        }}"#,
        item_key = escape_graphql_string(&item_key),
        requester_did = escape_graphql_string(&context.requester_did),
        node_did = escape_graphql_string(&context.node_did),
        kind = args.kind.as_str(),
        action = args.action.as_str(),
        title = escape_graphql_string(&args.title),
        summary = optional_string_field("summary", args.summary.as_deref()),
        payload = optional_string_field("payload", args.payload.as_deref()),
        source_kind = args.source_kind.as_str(),
        source_id = escape_graphql_string(&args.source_id),
        session_id = optional_string_field("session_id", args.session_id.as_deref()),
        request_id = optional_string_field("request_id", args.request_id.as_deref()),
        cause_doc_id = optional_string_field("cause_doc_id", args.cause_doc_id.as_deref()),
        target_node_did = escape_graphql_string(&context.node_did),
        target_agent_id = escape_graphql_string(&context.agent_id),
        expected_collection =
            optional_string_field("expected_collection", args.expected_collection.as_deref()),
        parent_item_id = optional_string_field("parent_item_id", args.parent_item_id.as_deref()),
        deadline_at = optional_string_field("deadline_at", args.deadline_at.as_deref()),
        now = escape_graphql_string(&now),
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "mailbox.create_stamped_item",
        &mutation,
    )
    .await;
    if let Ok(response) = &response {
        let document = single_mutation_document(response, "create_MailboxItem")?
            .context("create stamped mailbox item returned no row")?;
        return Ok(MailboxWriteReceipt {
            outcome: MailboxWriteOutcome::Created,
            item: serde_json::from_value(document.clone()).context("decode created MailboxItem")?,
        });
    }

    // A local unique-index race is idempotent only if the exact key resolves
    // to the same owner/source tuple; any other collision fails closed.
    if let Some(item) = load_mailbox_item_by_key(node, &item_key).await? {
        if item.requester_did == context.requester_did
            && item.parsed_status() == Some(MailboxStatus::Open)
            && item.source_kind == args.source_kind.as_str()
            && item.source_id == args.source_id
            && item.kind == args.kind.as_str()
        {
            return notification::reuse_or_update(node, context, &args, identity, &item).await;
        }
        bail!("mailbox unique-key collision did not match an open stamped owner/source tuple");
    }
    Err(response.expect_err("successful mailbox write returned above"))
        .context("create stamped mailbox item")
}

#[derive(Debug)]
pub struct MailboxToolError(anyhow::Error);

impl std::fmt::Display for MailboxToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for MailboxToolError {}

impl From<anyhow::Error> for MailboxToolError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

#[derive(Clone)]
pub struct MailboxCreateTool {
    node: Arc<EmbeddedNode>,
    policy: MailboxNotificationPolicy,
}

impl MailboxCreateTool {
    pub fn new(node: Arc<EmbeddedNode>, policy: MailboxNotificationPolicy) -> Self {
        Self { node, policy }
    }
}

impl crate::llm::tool::Tool for MailboxCreateTool {
    const NAME: &'static str = FILE_MAILBOX_ITEM_TOOL_NAME;
    type Error = MailboxToolError;
    type Args = MailboxContentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: FILE_MAILBOX_ITEM_TOOL_NAME.to_string(),
            description: format!(
                "{} Configured policy: {}. Supply findings only; never supply IDs or routing. Receipt reports created, reused, or updated and the stored item. To ask the person to decide, pass `question` instead of `payload`: the item becomes an `ask`; an answer or explicit optional Skip arrives later in this session through its original signed reply. Filing never waits: keep working if you can proceed without the answer, or end your turn to wait for it.",
                canonical_mailbox_write_decl().description,
                serde_json::to_string(&self.policy).expect("serialize policy")
            ),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "payload": {"type": "string"},
                    "question": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "version": {"type": "integer", "enum": [1, 2, gents_protocol::mailbox_question::MAILBOX_QUESTION_VERSION], "description": "Use 1 for ordinary choices, 2 for optional Skip or free-text-only questions, and 3 for editable delegation prompts or suggested ranks. The recipient must support the chosen version."},
                            "prompt": {"type": "string"},
                            "options": {
                                "type": "array",
                                "minItems": 0,
                                "description": "Use two to four choices, or an empty list with allow_free_text for a free-text-only question.",
                                "maxItems": gents_protocol::mailbox_question::MAILBOX_QUESTION_MAX_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "id": {"type": "string"},
                                        "label": {"type": "string"},
                                        "description": {"type": "string", "description": "Short rationale and tradeoffs."},
                                        "prompt": {"type": "string", "description": "Version 3: proposed editable worker prompt. Requires allow_free_text and a single-select question; answering is a preference, not permission or direct worker dispatch."}
                                    },
                                    "required": ["id", "label"]
                                }
                            },
                            "multi_select": {"type": "boolean"},
                            "recommended_order": {"type": "array", "items": {"type": "string"}, "description": "Version 3 ranked question: every option id once, best first. Requires multi_select. Suggestions never select or submit an answer."},
                            "optional": {"type": "boolean", "description": "Allow an explicit Skip. Omit or set false when an answer is required."},
                            "allow_free_text": {"type": "boolean", "description": "Offer an \"Other\" free-text answer."}
                        },
                        "required": ["version", "prompt", "options"]
                    }
                },
                "required": ["title"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .context("file_mailbox_item requires a current AgentRequest context")?;
        let request_id = runtime.request_id.context("missing request_id")?;
        let context = MailboxStampContext {
            requester_did: runtime.requester_did.context("missing requester_did")?,
            node_did: runtime.node_did.context("missing node_did")?,
            agent_id: runtime.agent_id.context("missing agent_id")?,
            session_id: runtime.session_id,
        };
        self.policy.validate()?;
        let question = QuestionFiling::from_args(&args, &request_id)?;
        let request = notification::request_provenance(&self.node, &request_id, &context).await?;
        let (identity, event_id, kind, action, expected_collection, payload) = match &question {
            Some(question) => (
                &NotificationIdentity::Event,
                question.event_id.as_str(),
                MailboxKind::Ask,
                MailboxAction::StartRequest,
                None,
                Some(question.payload.clone()),
            ),
            None => (
                &self.policy.identity,
                request_id.as_str(),
                self.policy.kind,
                self.policy.action,
                self.policy.expected_collection.clone(),
                args.payload,
            ),
        };
        let source_id = identity.source_id(
            &context.node_did,
            &context.requester_did,
            &context.agent_id,
            event_id,
        )?;
        let receipt = stamp_notification(
            &self.node,
            &context,
            FileMailboxItemArgs {
                kind,
                action,
                title: args.title,
                summary: args.summary,
                payload,
                source_kind: MailboxSourceKind::Agent,
                source_id,
                session_id: None,
                request_id: Some(request_id),
                cause_doc_id: request.caused_by_source_doc_id,
                expected_collection,
                parent_item_id: None,
                deadline_at: None,
            },
            identity,
            MAILBOX_CLOSE_COLLECTIONS,
        )
        .await?;
        Ok(crate::tool_output::render(&receipt, &["item", "outcome"])
            .context("serialize notification receipt")?)
    }
}

struct QuestionFiling {
    event_id: String,
    payload: String,
}

impl QuestionFiling {
    fn from_args(args: &MailboxContentArgs, request_id: &str) -> Result<Option<Self>> {
        let Some(question) = &args.question else {
            return Ok(None);
        };
        anyhow::ensure!(
            nonempty(args.payload.clone()).is_none(),
            "a question is the item's payload; omit `payload` when passing `question`"
        );
        question.validate()?;
        let payload = serde_json::to_string(question).context("serialize mailbox question")?;
        use sha2::Digest;
        let digest = sha2::Sha256::digest(
            serde_json::to_vec(&(args.title.trim(), &payload)).context("digest question")?,
        );
        let question_key: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Some(Self {
            event_id: format!("{request_id}:question:{question_key}"),
            payload,
        }))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MailboxCloseCollection {
    pub collection: &'static str,
    pub correlation_field: &'static str,
}

pub const MAILBOX_CLOSE_COLLECTIONS: &[MailboxCloseCollection] = &[];

fn mailbox_close_collection(
    entries: &[MailboxCloseCollection],
    name: &str,
) -> Option<MailboxCloseCollection> {
    entries
        .iter()
        .copied()
        .find(|entry| entry.collection == name)
}

pub fn validate_close_collections(entries: &[MailboxCloseCollection]) -> Result<()> {
    for entry in entries {
        validate_collection_identifier(entry.collection)?;
        validate_graphql_name(entry.correlation_field)?;
    }
    Ok(())
}

pub async fn list_mailbox_items(
    node: &EmbeddedNode,
    requester_did: &str,
    status: Option<MailboxStatus>,
) -> Result<Vec<MailboxItem>> {
    if requester_did.trim().is_empty() {
        bail!("mailbox list requires requester DID");
    }
    let status_filter = status
        .map(|status| format!(r#", status: {{ _eq: "{}" }}"#, status.as_str()))
        .unwrap_or_default();
    let query = format!(
        r#"{{
            MailboxItem(
                filter: {{ requester_did: {{ _eq: "{}" }}{status_filter} }},
                order: {{ created_at: DESC }}
            ) {{ {MAILBOX_FIELDS} }}
        }}"#,
        escape_graphql_string(requester_did)
    );
    let response = graphql_with_transaction_retry(node, &query, "list mailbox items").await?;
    Ok(rows(&response, MAILBOX_COLLECTION)?)
}

async fn transition_open_item(
    node: &std::sync::Arc<EmbeddedNode>,
    item: &MailboxItem,
    target: MailboxStatus,
    resolved_doc_id: Option<&str>,
) -> Result<bool> {
    if !target.is_terminal() {
        bail!("mailbox transition target must be terminal");
    }
    let now = Utc::now().to_rfc3339();
    let resolved_doc_id = optional_string_field("resolved_doc_id", resolved_doc_id);
    let mutation = format!(
        r#"mutation {{
            update_MailboxItem(
                filter: {{ _docID: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }},
                input: {{
                    status: "{}",
                    updated_at: "{}",
                    resolved_at: "{}",
                    {resolved_doc_id}
                }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(&item.doc_id),
        target.as_str(),
        escape_graphql_string(&now),
        escape_graphql_string(&now),
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "mailbox.close_item",
        &mutation,
    )
    .await?;
    Ok(single_mutation_document(&response, "update_MailboxItem")?.is_some())
}

async fn domain_document_satisfying_item(
    node: &EmbeddedNode,
    item: &MailboxItem,
    entries: &[MailboxCloseCollection],
) -> Result<Option<String>> {
    let Some(expected) = item.expected_collection.as_deref() else {
        bail!("write_document MailboxItem is missing expected_collection");
    };
    let field = domain::response_field(node, expected, entries)?;
    let query = format!(
        r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{item_key}" }} }}, limit: 2) {{ _docID }} }}"#,
        collection = expected,
        item_key = escape_graphql_string(&item.item_key),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "find mailbox-correlated document").await?;
    let found = rows::<Value>(&response, expected)?;
    if found.len() > 1 {
        bail!(
            "more than one {expected} row satisfies mailbox key {}",
            item.item_key
        );
    }
    Ok(found
        .first()
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MailboxSweepReport {
    pub scanned: usize,
    pub acted: usize,
    pub expired: usize,
    pub skipped_unsupported: usize,
    pub skipped_errors: usize,
}

pub(crate) struct MailboxSweepPage {
    pub report: MailboxSweepReport,
    pub next_cursor: Option<String>,
}

const MAILBOX_SWEEP_PAGE_SIZE: usize = 16;
const MAILBOX_SWEEP_TIME_BUDGET: Duration = Duration::from_millis(500);

pub(crate) async fn sweep_open_mailbox_items(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
    after_doc_id: Option<&str>,
) -> Result<MailboxSweepPage> {
    sweep_open_mailbox_items_with_close_collections(
        node,
        local_did,
        after_doc_id,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
}

// Keep this complete order aligned with the ordinary index: Defra only seeks
// cursor pages when the declared order is fully covered by one index.
fn open_mailbox_page_query(local_did: &str, after_cursor: Option<&str>) -> String {
    let after_arg = after_cursor
        .map(|cursor| format!(", after: \"{}\"", escape_graphql_string(cursor)))
        .unwrap_or_default();
    format!(
        r#"{{ _cursor {{ MailboxItem(
            first: {MAILBOX_SWEEP_PAGE_SIZE}{after_arg},
            filter: {{ node_did: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }},
            order: [{{ node_did: ASC }}, {{ status: ASC }}, {{ item_key: ASC }}]
        ) {{ {MAILBOX_FIELDS} }}
        _pageInfo {{ hasNext }} }} }}"#,
        escape_graphql_string(local_did),
    )
}

fn mailbox_item_cursor(raw_item: &Value) -> Result<String> {
    let doc_id = raw_item
        .get("_docID")
        .and_then(Value::as_str)
        .context("mailbox sweep row lacks physical _docID")?;
    let keys = BTreeMap::from([
        (
            "item_key".to_owned(),
            raw_item
                .get("item_key")
                .cloned()
                .context("mailbox cursor row omitted ordered item_key")?,
        ),
        (
            "node_did".to_owned(),
            raw_item
                .get("node_did")
                .cloned()
                .context("mailbox cursor row omitted ordered node_did")?,
        ),
        (
            "status".to_owned(),
            raw_item
                .get("status")
                .cloned()
                .context("mailbox cursor row omitted ordered status")?,
        ),
    ]);
    Ok(cursor::Cursor {
        doc_id: doc_id.to_owned(),
        keys,
        direction: String::new(),
    }
    .encode())
}

async fn sweep_open_mailbox_items_with_close_collections(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
    after_cursor: Option<&str>,
    close_collections: &[MailboxCloseCollection],
) -> Result<MailboxSweepPage> {
    sweep_open_mailbox_items_with_clock(
        node,
        local_did,
        after_cursor,
        close_collections,
        Instant::now,
    )
    .await
}

async fn sweep_open_mailbox_items_with_clock(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
    after_cursor: Option<&str>,
    close_collections: &[MailboxCloseCollection],
    read_clock: impl Fn() -> Instant,
) -> Result<MailboxSweepPage> {
    validate_close_collections(close_collections)?;
    let response = graphql_with_transaction_retry(
        node,
        &open_mailbox_page_query(local_did, after_cursor),
        "load open mailbox items",
    )
    .await?;
    let cursor_page = response
        .data
        .context("mailbox cursor query omitted data")?
        .get("_cursor")
        .context("mailbox cursor query omitted _cursor")?
        .clone();
    let raw_items: Vec<Value> = serde_json::from_value(
        cursor_page
            .get(MAILBOX_COLLECTION)
            .context("mailbox cursor query omitted collection rows")?
            .clone(),
    )
    .context("decode MailboxItem cursor rows")?;
    anyhow::ensure!(
        raw_items.len() <= MAILBOX_SWEEP_PAGE_SIZE,
        "mailbox cursor query exceeded requested page size"
    );
    let has_more = cursor_page
        .get("_pageInfo")
        .and_then(|page_info| page_info.get("hasNext"))
        .and_then(Value::as_bool)
        .context("mailbox cursor query omitted _pageInfo.hasNext")?;
    anyhow::ensure!(
        !has_more || !raw_items.is_empty(),
        "mailbox cursor query hasNext without returning rows"
    );
    let mut report = MailboxSweepReport {
        scanned: 0,
        ..Default::default()
    };
    let now = Utc::now();
    let turn_started = read_clock();
    let mut last_processed_cursor = None;
    let mut stopped_early = false;
    for raw_item in raw_items {
        if report.scanned > 0
            && read_clock().duration_since(turn_started) >= MAILBOX_SWEEP_TIME_BUDGET
        {
            stopped_early = true;
            break;
        }
        let item_cursor = mailbox_item_cursor(&raw_item)?;
        let item_id = raw_item
            .get("_docID")
            .and_then(Value::as_str)
            .context("mailbox sweep row lacks physical _docID")?
            .to_owned();
        report.scanned += 1;
        last_processed_cursor = Some(item_cursor);
        let item = match serde_json::from_value::<MailboxItem>(raw_item) {
            Ok(item) => item,
            Err(error) => {
                report.skipped_errors += 1;
                tracing::warn!(%item_id, %error, "mailbox item decode failed; leaving item open");
                continue;
            }
        };
        let Some(action) = item.parsed_action() else {
            report.skipped_unsupported += 1;
            tracing::warn!(
                item_id = %item.doc_id,
                action = %item.action,
                "leaving mailbox item with unsupported action open"
            );
            continue;
        };
        let satisfying = match action {
            // The request claim transaction owns authenticated reply consumption.
            MailboxAction::StartRequest => Ok(None),
            MailboxAction::WriteDocument => {
                domain_document_satisfying_item(node, &item, close_collections).await
            }
            MailboxAction::Ack => Ok(None),
        };
        let satisfying = match satisfying {
            Ok(found) => found,
            Err(error) => {
                report.skipped_errors += 1;
                tracing::warn!(item_id = %item.doc_id, %error, "mailbox item close check failed; leaving item open");
                continue;
            }
        };
        if let Some(doc_id) = satisfying {
            match transition_open_item(node, &item, MailboxStatus::Acted, Some(&doc_id)).await {
                Ok(true) => report.acted += 1,
                Ok(false) => {}
                Err(error) => {
                    report.skipped_errors += 1;
                    tracing::warn!(item_id = %item.doc_id, %error, "mailbox item acted transition failed; leaving item open");
                }
            }
            continue;
        }
        let deadline_due = item
            .deadline_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|deadline| deadline.with_timezone(&Utc) <= now);
        if deadline_due {
            match transition_open_item(node, &item, MailboxStatus::Expired, None).await {
                Ok(true) => report.expired += 1,
                Ok(false) => {}
                Err(error) => {
                    report.skipped_errors += 1;
                    tracing::warn!(item_id = %item.doc_id, %error, "mailbox item expiry transition failed; leaving item open");
                }
            }
        }
    }
    let next_cursor = if stopped_early || has_more {
        last_processed_cursor
    } else {
        None
    };
    Ok(MailboxSweepPage {
        report,
        next_cursor,
    })
}

#[cfg(test)]
mod tests;
