//! Durable human-attention index.
//!
//! Mailbox rows are stamped envelopes. They do not advance graph work: only
//! the correlated `AgentRequest` or domain-document create carries semantics.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::document_config::{WriteToolDecl, WriteToolField};
use crate::graphql::{
    ensure_no_errors, escape_graphql_string, graphql_mutation_response_with_transaction_retry,
    graphql_mutation_with_transaction_retry, graphql_with_transaction_retry, rows,
    single_mutation_document, validate_collection_identifier, validate_graphql_name,
};
use crate::llm::tool::ToolDefinition;

pub const MAILBOX_COLLECTION: &str = "MailboxItem";
pub const FILE_MAILBOX_ITEM_TOOL_NAME: &str = "file_mailbox_item";

pub const MAILBOX_FIELDS: &str = r#"
    _docID
    item_key
    requester_did
    agent_did
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
    graph_run_id
    cause_doc_id
    target_agent_did
    target_behavior_id
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
    Graph,
    Session,
    Agent,
    Runtime,
    Tool,
}

impl MailboxSourceKind {
    pub const ALL: [Self; 5] = [
        Self::Graph,
        Self::Session,
        Self::Agent,
        Self::Runtime,
        Self::Tool,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
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
    pub agent_did: String,
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
    pub graph_run_id: Option<String>,
    #[serde(default)]
    pub cause_doc_id: Option<String>,
    pub target_agent_did: String,
    pub target_behavior_id: String,
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
    pub agent_did: String,
    pub behavior_id: String,
    pub session_id: Option<String>,
}

impl MailboxStampContext {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("requester_did", self.requester_did.as_str()),
            ("agent_did", self.agent_did.as_str()),
            ("behavior_id", self.behavior_id.as_str()),
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
    pub graph_run_id: Option<String>,
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

/// The only declaration allowed to target `MailboxItem`.
pub fn canonical_mailbox_write_decl() -> WriteToolDecl {
    WriteToolDecl {
        tool_name: FILE_MAILBOX_ITEM_TOOL_NAME.to_string(),
        collection: MAILBOX_COLLECTION.to_string(),
        description: "File a stamped item in the current requester's mailbox.".to_string(),
        fields: [
            ("kind", true),
            ("action", true),
            ("title", true),
            ("summary", false),
            ("payload", false),
            ("source_kind", true),
            ("source_id", true),
            ("session_id", false),
            ("request_id", false),
            ("graph_run_id", false),
            ("cause_doc_id", false),
            ("expected_collection", false),
            ("parent_item_id", false),
            ("deadline_at", false),
        ]
        .into_iter()
        .map(|(name, required)| canonical_field(name, required))
        .collect(),
        output_obligation: None,
    }
}

pub fn validate_mailbox_write_decl(decl: &WriteToolDecl) -> Result<()> {
    if decl.collection != MAILBOX_COLLECTION {
        return Ok(());
    }
    if decl != &canonical_mailbox_write_decl() {
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
    rows(&response, MAILBOX_COLLECTION)
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
    close_collections: &[MailboxCloseCollection],
) -> Result<()> {
    context.validate()?;
    args.title = args.title.trim().to_string();
    args.source_id = args.source_id.trim().to_string();
    args.summary = nonempty(args.summary.take());
    args.payload = nonempty(args.payload.take());
    args.session_id = nonempty(args.session_id.take()).or_else(|| context.session_id.clone());
    args.request_id = nonempty(args.request_id.take());
    args.graph_run_id = nonempty(args.graph_run_id.take());
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
            if mailbox_close_collection(close_collections, expected).is_none() {
                bail!("expected_collection {expected:?} is not allowed for mailbox close");
            }
        }
        MailboxAction::Ack | MailboxAction::StartRequest => {
            if args.expected_collection.is_some() {
                bail!("{} must not set expected_collection", args.action.as_str());
            }
        }
    }
    Ok(())
}

/// Stamp and persist a mailbox item. The model never supplies owner, acting
/// principal, target route, lifecycle state, timestamps, or item key.
pub async fn stamp_create(
    node: &EmbeddedNode,
    context: &MailboxStampContext,
    args: FileMailboxItemArgs,
) -> Result<MailboxItem> {
    stamp_create_with_close_collections(node, context, args, MAILBOX_CLOSE_COLLECTIONS).await
}

pub async fn stamp_create_with_close_collections(
    node: &EmbeddedNode,
    context: &MailboxStampContext,
    mut args: FileMailboxItemArgs,
    close_collections: &[MailboxCloseCollection],
) -> Result<MailboxItem> {
    validate_close_collections(close_collections)?;
    validate_file_args(&mut args, context, close_collections)?;
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
        [item] if item.requester_did == context.requester_did => return Ok((*item).clone()),
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
                agent_did: "{agent_did}",
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
                {graph_run_id}
                {cause_doc_id}
                target_agent_did: "{target_agent_did}",
                target_behavior_id: "{target_behavior_id}",
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
        agent_did = escape_graphql_string(&context.agent_did),
        kind = args.kind.as_str(),
        action = args.action.as_str(),
        title = escape_graphql_string(&args.title),
        summary = optional_string_field("summary", args.summary.as_deref()),
        payload = optional_string_field("payload", args.payload.as_deref()),
        source_kind = args.source_kind.as_str(),
        source_id = escape_graphql_string(&args.source_id),
        session_id = optional_string_field("session_id", args.session_id.as_deref()),
        request_id = optional_string_field("request_id", args.request_id.as_deref()),
        graph_run_id = optional_string_field("graph_run_id", args.graph_run_id.as_deref()),
        cause_doc_id = optional_string_field("cause_doc_id", args.cause_doc_id.as_deref()),
        target_agent_did = escape_graphql_string(&context.agent_did),
        target_behavior_id = escape_graphql_string(&context.behavior_id),
        expected_collection =
            optional_string_field("expected_collection", args.expected_collection.as_deref()),
        parent_item_id = optional_string_field("parent_item_id", args.parent_item_id.as_deref()),
        deadline_at = optional_string_field("deadline_at", args.deadline_at.as_deref()),
        now = escape_graphql_string(&now),
    );
    let response = graphql_mutation_response_with_transaction_retry(
        node,
        &mutation,
        "create stamped mailbox item",
    )
    .await;
    if !response.has_errors() {
        let document = single_mutation_document(&response, "create_MailboxItem")?
            .context("create stamped mailbox item returned no row")?;
        return serde_json::from_value(document.clone()).context("decode created MailboxItem");
    }

    // A local unique-index race is idempotent only if the exact key resolves
    // to the same owner/source tuple. Any other collision fails closed.
    if let Some(item) = load_mailbox_item_by_key(node, &item_key).await? {
        if item.requester_did == context.requester_did
            && item.parsed_status() == Some(MailboxStatus::Open)
            && item.source_kind == args.source_kind.as_str()
            && item.source_id == args.source_id
            && item.kind == args.kind.as_str()
        {
            return Ok(item);
        }
        bail!("mailbox unique-key collision did not match an open stamped owner/source tuple");
    }
    ensure_no_errors(&response, "create stamped mailbox item")?;
    unreachable!("error response was checked")
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
}

impl MailboxCreateTool {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }
}

impl crate::llm::tool::Tool for MailboxCreateTool {
    const NAME: &'static str = FILE_MAILBOX_ITEM_TOOL_NAME;
    type Error = MailboxToolError;
    type Args = FileMailboxItemArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: FILE_MAILBOX_ITEM_TOOL_NAME.to_string(),
            description: canonical_mailbox_write_decl().description,
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {"type": "string", "enum": MailboxKind::ALL.map(MailboxKind::as_str)},
                    "action": {"type": "string", "enum": MailboxAction::ALL.map(MailboxAction::as_str)},
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "payload": {"type": "string"},
                    "source_kind": {"type": "string", "enum": MailboxSourceKind::ALL.map(MailboxSourceKind::as_str)},
                    "source_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "request_id": {"type": "string"},
                    "graph_run_id": {"type": "string"},
                    "cause_doc_id": {"type": "string"},
                    "expected_collection": {"type": "string"},
                    "parent_item_id": {"type": "string"},
                    "deadline_at": {"type": "string", "description": "RFC3339 deadline"}
                },
                "required": ["kind", "action", "title", "source_kind", "source_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .context("file_mailbox_item requires a current AgentRequest context")?;
        let item = stamp_create(
            &self.node,
            &MailboxStampContext {
                requester_did: runtime.requester_did.context("missing requester_did")?,
                agent_did: runtime.agent_did.context("missing agent_did")?,
                behavior_id: runtime.behavior_id.context("missing behavior_id")?,
                session_id: runtime.session_id,
            },
            args,
        )
        .await?;
        Ok(format!(
            "filed MailboxItem {} ({})",
            item.doc_id, item.item_key
        ))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MailboxCloseCollection {
    pub collection: &'static str,
    pub correlation_field: &'static str,
}

/// Domain-document close engines are registered only alongside a schema that
/// carries the immutable correlation field. This platform slice has none.
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
    rows(&response, MAILBOX_COLLECTION)
}

pub async fn dismiss_mailbox_item(
    node: &EmbeddedNode,
    doc_id: &str,
    principal_did: &str,
) -> Result<MailboxItem> {
    let item = load_mailbox_item(node, doc_id)
        .await?
        .context("MailboxItem not found")?;
    if item.requester_did != principal_did.trim() {
        bail!("only requester_did may dismiss a MailboxItem");
    }
    if item.parsed_status().is_some_and(MailboxStatus::is_terminal) {
        return Ok(item);
    }
    if item.parsed_status() != Some(MailboxStatus::Open) {
        bail!("MailboxItem has unknown status {:?}", item.status);
    }
    transition_open_item(node, &item, MailboxStatus::Dismissed, None).await?;
    load_mailbox_item(node, doc_id)
        .await?
        .context("dismissed MailboxItem disappeared")
}

async fn transition_open_item(
    node: &EmbeddedNode,
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
    let response =
        graphql_mutation_with_transaction_retry(node, &mutation, "close mailbox item").await?;
    Ok(single_mutation_document(&response, "update_MailboxItem")?.is_some())
}

async fn request_satisfying_item(
    node: &EmbeddedNode,
    item: &MailboxItem,
) -> Result<Option<String>> {
    let session_filter = item
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                r#"session_id: {{ _eq: "{}" }},"#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{ AgentRequest(filter: {{
            caused_by_source_doc_id: {{ _eq: "{}" }},
            execution_origin: {{ _eq: "interactive" }},
            requester_did: {{ _eq: "{}" }},
            agent_did: {{ _eq: "{}" }},
            behavior_id: {{ _eq: "{}" }},
            {session_filter}
        }}, limit: 1) {{ _docID }} }}"#,
        escape_graphql_string(&item.doc_id),
        escape_graphql_string(&item.requester_did),
        escape_graphql_string(&item.target_agent_did),
        escape_graphql_string(&item.target_behavior_id),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "find mailbox-caused request").await?;
    let found = rows::<Value>(&response, "AgentRequest")?;
    Ok(found
        .first()
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn domain_document_satisfying_item(
    node: &EmbeddedNode,
    item: &MailboxItem,
    entries: &[MailboxCloseCollection],
) -> Result<Option<String>> {
    let Some(expected) = item.expected_collection.as_deref() else {
        bail!("write_document MailboxItem is missing expected_collection");
    };
    let Some(entry) = entries.iter().find(|entry| entry.collection == expected) else {
        bail!("unsupported mailbox expected_collection {expected:?}");
    };
    let query = format!(
        r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{item_key}" }} }}, limit: 2) {{ _docID }} }}"#,
        collection = entry.collection,
        field = entry.correlation_field,
        item_key = escape_graphql_string(&item.item_key),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "find mailbox-correlated document").await?;
    let found = rows::<Value>(&response, entry.collection)?;
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

pub async fn sweep_open_mailbox_items(node: &EmbeddedNode) -> Result<MailboxSweepReport> {
    sweep_open_mailbox_items_with_close_collections(node, MAILBOX_CLOSE_COLLECTIONS).await
}

pub async fn sweep_open_mailbox_items_with_close_collections(
    node: &EmbeddedNode,
    close_collections: &[MailboxCloseCollection],
) -> Result<MailboxSweepReport> {
    validate_close_collections(close_collections)?;
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ MailboxItem(filter: {{ status: {{ _eq: "open" }} }}) {{ {MAILBOX_FIELDS} }} }}"#
        ),
        "load open mailbox items",
    )
    .await?;
    let raw_items = rows::<Value>(&response, MAILBOX_COLLECTION)?;
    let mut report = MailboxSweepReport {
        scanned: raw_items.len(),
        ..Default::default()
    };
    let now = Utc::now();
    for raw_item in raw_items {
        let item_id = raw_item
            .get("_docID")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
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
            MailboxAction::StartRequest => request_satisfying_item(node, &item).await,
            MailboxAction::WriteDocument => {
                domain_document_satisfying_item(node, &item, close_collections).await
            }
            MailboxAction::Ack => Ok(None),
        };
        let satisfying = match satisfying {
            Ok(found) => found,
            Err(error)
                if error
                    .to_string()
                    .contains("unsupported mailbox expected_collection") =>
            {
                report.skipped_unsupported += 1;
                tracing::warn!(item_id = %item.doc_id, %error, "leaving unsupported mailbox item open");
                continue;
            }
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
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::EmbeddedNode;

    use super::*;
    use crate::identity::AgentIdentity;

    const FIXTURE_CLOSE: &[MailboxCloseCollection] = &[MailboxCloseCollection {
        collection: "MailboxFixture",
        correlation_field: "mailbox_item_key",
    }];

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node.add_schema(
            r#"
            type MailboxFixture {
                mailbox_item_key: String @index(unique: true) @immutable
                body: String
            }
            "#,
        )
        .await
        .unwrap();
        node
    }

    fn context(owner: &str) -> MailboxStampContext {
        MailboxStampContext {
            requester_did: owner.into(),
            agent_did: "did:test:agent".into(),
            behavior_id: "operator".into(),
            session_id: Some("session-1".into()),
        }
    }

    fn args(action: MailboxAction, source_id: &str) -> FileMailboxItemArgs {
        FileMailboxItemArgs {
            kind: MailboxKind::Ask,
            action,
            title: "Review the result".into(),
            summary: Some("A durable attention envelope".into()),
            payload: None,
            source_kind: MailboxSourceKind::Graph,
            source_id: source_id.into(),
            session_id: None,
            request_id: None,
            graph_run_id: Some("run-1".into()),
            cause_doc_id: None,
            expected_collection: (action == MailboxAction::WriteDocument)
                .then(|| "MailboxFixture".into()),
            parent_item_id: None,
            deadline_at: None,
        }
    }

    #[tokio::test]
    async fn stamping_is_owner_scoped_idempotent_and_never_reopens() {
        let node = test_node().await;
        let first = stamp_create(
            &node,
            &context("did:test:owner-a"),
            args(MailboxAction::Ack, "wait-1"),
        )
        .await
        .unwrap();
        let retry = stamp_create(
            &node,
            &context("did:test:owner-a"),
            args(MailboxAction::Ack, "wait-1"),
        )
        .await
        .unwrap();
        assert_eq!(first.doc_id, retry.doc_id);
        assert_eq!(first.item_key, "graph:wait-1:ask:1");

        let collision = stamp_create(
            &node,
            &context("did:test:owner-b"),
            args(MailboxAction::Ack, "wait-1"),
        )
        .await
        .unwrap_err();
        assert!(collision.to_string().contains("collision"));

        assert!(dismiss_mailbox_item(&node, &first.doc_id, "did:test:other")
            .await
            .is_err());
        dismiss_mailbox_item(&node, &first.doc_id, "did:test:owner-a")
            .await
            .unwrap();
        let second = stamp_create(
            &node,
            &context("did:test:owner-a"),
            args(MailboxAction::Ack, "wait-1"),
        )
        .await
        .unwrap();
        assert_eq!(second.item_key, "graph:wait-1:ask:2");
        assert_ne!(second.doc_id, first.doc_id);

        let duplicate_open = format!(
            r#"mutation {{ create_MailboxItem(input: {{
                item_key: "graph:wait-1:ask:3", requester_did: "did:test:owner-a",
                agent_did: "did:test:agent", status: "open", kind: "ask", action: "ack",
                title: "duplicate", summary: null, payload: null, source_kind: "graph",
                source_id: "wait-1", session_id: null, request_id: null, graph_run_id: null,
                cause_doc_id: null, target_agent_did: "did:test:agent",
                target_behavior_id: "operator", expected_collection: null, parent_item_id: null,
                deadline_at: null, created_at: "{}", updated_at: "{}",
                resolved_at: null, resolved_doc_id: null
            }}) {{ _docID }} }}"#,
            Utc::now().to_rfc3339(),
            Utc::now().to_rfc3339()
        );
        let response = node.execute(&duplicate_open).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let error = stamp_create(
            &node,
            &context("did:test:owner-a"),
            args(MailboxAction::Ack, "wait-1"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("more than one"));
    }

    #[tokio::test]
    async fn stamping_rejects_missing_context_unknown_fields_and_unsupported_domain_routes() {
        let node = test_node().await;
        let mut missing = context("");
        missing.requester_did.clear();
        assert!(
            stamp_create(&node, &missing, args(MailboxAction::Ack, "wait-2"))
                .await
                .is_err()
        );
        assert!(serde_json::from_value::<FileMailboxItemArgs>(json!({
            "kind": "ask", "action": "ack", "title": "x", "source_kind": "graph",
            "source_id": "wait-2", "requester_did": "did:test:forged"
        }))
        .is_err());
        assert!(stamp_create(
            &node,
            &context("did:test:owner"),
            args(MailboxAction::WriteDocument, "wait-3"),
        )
        .await
        .is_err());
        assert!(validate_close_collections(&[MailboxCloseCollection {
            collection: "Bad) { hacked",
            correlation_field: "mailbox_item_key",
        }])
        .is_err());
    }

    #[tokio::test]
    async fn event_stage_materialization_persists_explicit_requester_lineage() {
        let key_path =
            std::env::temp_dir().join(format!("mailbox-event-stage-{}.key", uuid::Uuid::new_v4()));
        let identity = crate::identity::KeyIdentity::load_or_create(key_path, None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let trigger_context = serde_json::json!({
            "version": 1,
            "source_fields": {"requester_did": identity.did()}
        })
        .to_string();
        let parsed =
            crate::lifecycle::TriggerExecutionContext::parse(Some(&trigger_context)).unwrap();
        let requester_did = parsed
            .source_fields
            .get("requester_did")
            .map(String::as_str);
        let enqueued = crate::lifecycle::materialize::write_pending_agent_request_with_lineage_workspace_and_conversation_title(
            node.as_ref(),
            identity.did(),
            "operator",
            "continue the graph",
            crate::lifecycle::ExecutionOrigin::Scheduled,
            crate::lifecycle::TriggerLineage {
                trigger_id: Some("event-trigger-1".into()),
                trigger_kind: Some("event".into()),
                source_doc_id: Some("source-doc-1".into()),
                correlation: Some("run-1".into()),
                trigger_context: Some(trigger_context),
            },
            None,
            None,
            None,
            requester_did,
            Some("event-trigger-config-doc-1"),
        )
        .await
        .unwrap();
        let response = graphql_with_transaction_retry(
            node.as_ref(),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ requester_did caused_by_source_doc_id }} }}"#,
                escape_graphql_string(&enqueued.doc_id)
            ),
            "verify event-stage owner lineage",
        )
        .await
        .unwrap();
        let rows = rows::<Value>(&response, "AgentRequest").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["requester_did"], identity.did());
        assert_eq!(rows[0]["caused_by_source_doc_id"], "source-doc-1");
    }

    #[tokio::test]
    async fn close_engines_are_disjoint_and_satisfying_create_wins_over_expiry() {
        let node = test_node().await;
        let observed_item = stamp_create(
            &node,
            &context("did:test:owner"),
            args(MailboxAction::StartRequest, "wait-observed"),
        )
        .await
        .unwrap();
        let event_child = format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "mailbox-observer-child", agent_did: "did:test:agent",
                requester_did: "did:test:owner", behavior_id: "operator",
                session_id: "session-1", content: "observe",
                lifecycle_state: "pending", execution_origin: "scheduled",
                caused_by_trigger_kind: "event", caused_by_source_doc_id: "{}",
                created_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&observed_item.doc_id),
            Utc::now().to_rfc3339()
        );
        assert!(!node.execute(&event_child).await.has_errors());
        let observer_report = sweep_open_mailbox_items_with_close_collections(&node, FIXTURE_CLOSE)
            .await
            .unwrap();
        assert_eq!(observer_report.acted, 0);
        assert_eq!(
            load_mailbox_item(&node, &observed_item.doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );

        let past = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        let mut request_args = args(MailboxAction::StartRequest, "wait-request");
        request_args.deadline_at = Some(past.clone());
        let request_item = stamp_create(&node, &context("did:test:owner"), request_args)
            .await
            .unwrap();
        let create_request = format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "mailbox-request", agent_did: "did:test:agent",
                requester_did: "did:test:owner", behavior_id: "operator",
                session_id: "session-1", content: "continue",
                lifecycle_state: "pending", execution_origin: "interactive",
                caused_by_source_doc_id: "{}", created_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&request_item.doc_id),
            Utc::now().to_rfc3339()
        );
        assert!(!node.execute(&create_request).await.has_errors());
        let report = sweep_open_mailbox_items_with_close_collections(&node, FIXTURE_CLOSE)
            .await
            .unwrap();
        assert_eq!(report.acted, 1);
        assert_eq!(report.expired, 0);

        let domain_args = args(MailboxAction::WriteDocument, "wait-domain");
        let domain_item = stamp_create_with_close_collections(
            &node,
            &context("did:test:owner"),
            domain_args,
            FIXTURE_CLOSE,
        )
        .await
        .unwrap();
        let unrelated_request = format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "transport-only",
                agent_did: "did:test:agent", requester_did: "did:test:owner",
                behavior_id: "operator", session_id: "session-1", content: "draft",
                caused_by_source_doc_id: "{}", created_at: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&domain_item.doc_id),
            Utc::now().to_rfc3339()
        );
        assert!(!node.execute(&unrelated_request).await.has_errors());
        let request_only = sweep_open_mailbox_items_with_close_collections(&node, FIXTURE_CLOSE)
            .await
            .unwrap();
        assert_eq!(request_only.acted, 0);
        assert_eq!(request_only.expired, 0);
        assert_eq!(
            load_mailbox_item(&node, &domain_item.doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
        let fixture = format!(
            r#"mutation {{ create_MailboxFixture(input: {{ mailbox_item_key: "{}", body: "done" }}) {{ _docID }} }}"#,
            escape_graphql_string(&domain_item.item_key)
        );
        assert!(!node.execute(&fixture).await.has_errors());
        let report = sweep_open_mailbox_items_with_close_collections(&node, FIXTURE_CLOSE)
            .await
            .unwrap();
        assert_eq!(report.acted, 1);
        assert_eq!(report.expired, 0);
        let closed = load_mailbox_item(&node, &domain_item.doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(closed.status, "acted");
        assert!(closed.resolved_doc_id.is_some());
    }

    #[tokio::test]
    async fn poisoned_item_does_not_block_later_expiry() {
        let node = test_node().await;
        let now = Utc::now().to_rfc3339();
        let hostile = format!(
            r#"mutation {{ create_MailboxItem(input: {{
                item_key: "graph:hostile:gate:1", requester_did: "did:test:owner",
                agent_did: "did:test:agent", status: "open", kind: "gate",
                action: "write_document", title: "hostile", source_kind: "graph",
                source_id: "hostile", target_agent_did: "did:test:agent",
                target_behavior_id: "operator", created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }} }}"#
        );
        assert!(!node.execute(&hostile).await.has_errors());
        let undecodable = format!(
            r#"mutation {{ create_MailboxItem(input: {{
                item_key: "graph:undecodable:gate:1", requester_did: "did:test:owner",
                agent_did: "did:test:agent", status: "open", kind: "gate",
                action: "ack", source_kind: "graph", source_id: "undecodable",
                target_agent_did: "did:test:agent", target_behavior_id: "operator",
                created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }} }}"#
        );
        assert!(!node.execute(&undecodable).await.has_errors());

        let mut expiring_args = args(MailboxAction::Ack, "wait-expiry");
        expiring_args.deadline_at = Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339());
        let expiring = stamp_create(&node, &context("did:test:owner"), expiring_args)
            .await
            .unwrap();

        let report = sweep_open_mailbox_items_with_close_collections(&node, FIXTURE_CLOSE)
            .await
            .unwrap();
        assert_eq!(report.skipped_errors, 2);
        assert_eq!(report.expired, 1);
        assert_eq!(
            load_mailbox_item(&node, &expiring.doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "expired"
        );
    }
}
