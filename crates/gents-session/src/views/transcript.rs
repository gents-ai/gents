use gents_protocol::document_query::{
    fields::{
        agent_message as message, agent_output_segment as segment, agent_tool_call as tool,
        compaction_entry as compaction,
    },
    kind, Direction, Field, Filter, JsonPath, Query, Selection,
};
use gents_protocol::output::{OutputSegment, OutputSource, TranscriptMessage};
use gents_protocol::session::SessionScope;
use gents_protocol::views::View;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone)]
pub enum Scope {
    Node(String),
    Session(SessionScope),
    Origin {
        node_did: String,
        requester_did: Option<String>,
    },
}

impl Scope {
    pub fn node_did(&self) -> &str {
        match self {
            Self::Node(node) => node,
            Self::Session(scope) => &scope.node_did,
            Self::Origin { node_did, .. } => node_did,
        }
    }

    pub fn filter<C>(
        &self,
        node: Field<C, kind::Str>,
        session: Field<C, kind::Str>,
        requester: Field<C, kind::Str>,
    ) -> Filter<C> {
        let filter = Filter::eq(node, self.node_did());
        let requester_did = match self {
            Self::Node(_) => return filter,
            Self::Session(scope) => &scope.requester_did,
            Self::Origin { requester_did, .. } => requester_did,
        };
        let filter = filter.and(match requester_did {
            Some(did) => Filter::eq(requester, did),
            None => Filter::is_null(requester),
        });
        match self {
            Self::Session(scope) => filter.and(Filter::eq(session, &scope.session_id)),
            Self::Node(_) | Self::Origin { .. } => filter,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum SequenceRange {
    #[default]
    All,
    At(i64),
    Before(i64),
    After(i64),
    Inclusive {
        from: Option<i64>,
        through: Option<i64>,
    },
}

impl SequenceRange {
    fn filter<C>(self, field: Field<C, kind::Int>) -> Option<Filter<C>> {
        match self {
            Self::All => None,
            Self::At(sequence) => Some(Filter::eq(field, sequence)),
            Self::Before(sequence) => Some(Filter::lt(field, sequence)),
            Self::After(sequence) => Some(Filter::gt(field, sequence)),
            Self::Inclusive { from, through } => match (from, through) {
                (Some(from), Some(through)) => {
                    Some(Filter::ge(field, from).and(Filter::le(field, through)))
                }
                (Some(from), None) => Some(Filter::ge(field, from)),
                (None, Some(through)) => Some(Filter::le(field, through)),
                (None, None) => None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub message: TranscriptMessage,
}

impl<'de> Deserialize<'de> for MessageRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let row = gents_protocol::output::rows::decode_transcript_message_row(&value)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            doc_id: row.doc_id,
            message: row.message,
        })
    }
}

impl MessageRow {
    pub fn into_canonical(self) -> gents_protocol::output::rows::TranscriptMessageRow {
        gents_protocol::output::rows::TranscriptMessageRow {
            doc_id: self.doc_id,
            message: self.message,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub segment: OutputSegment,
}

impl<'de> Deserialize<'de> for SegmentRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let row = gents_protocol::output::rows::decode_output_segment_row(&value)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            doc_id: row.doc_id,
            segment: row.segment,
        })
    }
}

impl SegmentRow {
    pub fn into_canonical(self) -> gents_protocol::output::rows::OutputSegmentRow {
        gents_protocol::output::rows::OutputSegmentRow {
            doc_id: self.doc_id,
            segment: self.segment,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageRows {
    pub scope: Scope,
    pub document_ids: Option<Vec<String>>,
    pub session_id: Option<String>,
    pub request_doc_ids: Option<Vec<String>>,
    pub message_key: Option<String>,
    pub role: Option<gents_protocol::output::MessageRole>,
    pub sequence: SequenceRange,
    pub direction: Direction,
    pub limit: Option<u64>,
}

impl MessageRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            document_ids: None,
            session_id: None,
            request_doc_ids: None,
            message_key: None,
            role: None,
            sequence: SequenceRange::All,
            direction: Direction::Asc,
            limit: None,
        }
    }

    fn select(&self, selection: Selection<message::Doc>) -> Query<message::Doc> {
        let mut query = Query::select(selection)
            .filter(self.scope.filter(
                message::NODE_DID,
                message::SESSION_ID,
                message::REQUESTER_DID,
            ))
            .order_by(message::SEQUENCE, self.direction)
            .order_by(message::MESSAGE_KEY, self.direction);
        if let Some(ids) = &self.document_ids {
            query = query.filter(Filter::any_of(message::DOC_ID, ids));
        }
        if let Some(session) = &self.session_id {
            query = query.filter(Filter::eq(message::SESSION_ID, session));
        }
        if let Some(ids) = &self.request_doc_ids {
            query = query.filter(Filter::any_of(message::REQUEST_DOC_ID, ids));
        }
        if let Some(key) = &self.message_key {
            query = query.filter(Filter::eq(message::MESSAGE_KEY, key));
        }
        if let Some(role) = self.role {
            let value = match role {
                gents_protocol::output::MessageRole::System => "system",
                gents_protocol::output::MessageRole::User => "user",
                gents_protocol::output::MessageRole::Assistant => "assistant",
            };
            query = query.filter(Filter::eq(message::ROLE, value));
        }
        if let Some(filter) = self.sequence.filter(message::SEQUENCE) {
            query = query.filter(filter);
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

impl View for MessageRows {
    type Collection = message::Doc;
    type Row = MessageRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        self.select(
            Selection::new()
                .field(message::DOC_ID)
                .field(message::MESSAGE_KEY)
                .field(message::SESSION_ID)
                .field(message::NODE_DID)
                .field(message::REQUESTER_DID)
                .field(message::REQUEST_DOC_ID)
                .field(message::PUBLICATION)
                .field(message::OUTCOME)
                .field(message::SEQUENCE)
                .field(message::ROLE)
                .field(message::NATIVE_ID)
                .field(message::BLOCKS)
                .field(message::CREATED_AT),
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessageIdentityRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_doc_id: Option<String>,
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub message_key: String,
    pub role: Option<String>,
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MessageIdentities(pub MessageRows);

impl View for MessageIdentities {
    type Collection = message::Doc;
    type Row = MessageIdentityRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        self.0.select(
            Selection::new()
                .field(message::DOC_ID)
                .field(message::REQUEST_DOC_ID)
                .field(message::NODE_DID)
                .field(message::SESSION_ID)
                .field(message::REQUESTER_DID)
                .field(message::MESSAGE_KEY)
                .field(message::ROLE)
                .field(message::SEQUENCE),
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessageSequenceRow {
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MessageSequences(pub MessageRows);

impl View for MessageSequences {
    type Collection = message::Doc;
    type Row = MessageSequenceRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        self.0.select(Selection::new().field(message::SEQUENCE))
    }
}

const SOURCE_KIND: JsonPath<segment::Doc, kind::Str> = JsonPath::new(segment::SOURCE, &["kind"]);
const SOURCE_SCOPE: JsonPath<segment::Doc, kind::Str> = JsonPath::new(segment::SOURCE, &["scope"]);
const SOURCE_TURN: JsonPath<segment::Doc, kind::Int> =
    JsonPath::new(segment::SOURCE, &["turn_index"]);
const SOURCE_TOOL: JsonPath<segment::Doc, kind::Str> =
    JsonPath::new(segment::SOURCE, &["tool_call_doc_id"]);
const SOURCE_KEY: JsonPath<segment::Doc, kind::Str> = JsonPath::new(segment::SOURCE, &["key"]);

#[derive(Debug, Clone)]
pub struct SegmentRows {
    pub scope: Scope,
    pub document_ids: Option<Vec<String>>,
    pub request_doc_ids: Option<Vec<String>>,
    pub sources: Vec<OutputSource>,
    pub provider_live_only: bool,
    pub limit: Option<u64>,
}

impl SegmentRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            document_ids: None,
            request_doc_ids: None,
            sources: Vec::new(),
            provider_live_only: false,
            limit: None,
        }
    }
}

impl View for SegmentRows {
    type Collection = segment::Doc;
    type Row = SegmentRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(segment::DOC_ID)
                .field(segment::NODE_DID)
                .field(segment::REQUESTER_DID)
                .field(segment::SESSION_ID)
                .field(segment::REQUEST_DOC_ID)
                .field(segment::SOURCE)
                .field(segment::ORDINAL)
                .field(segment::WRITER)
                .field(segment::RUNS)
                .field(segment::PAYLOAD)
                .field(segment::CLOSE)
                .field(segment::CREATED_AT),
        )
        .filter(self.scope.filter(
            segment::NODE_DID,
            segment::SESSION_ID,
            segment::REQUESTER_DID,
        ))
        .order_by(segment::ORDINAL, Direction::Asc)
        .order_by(segment::DOC_ID, Direction::Asc);
        if let Some(ids) = &self.document_ids {
            query = query.filter(Filter::any_of(segment::DOC_ID, ids));
        }
        if let Some(ids) = &self.request_doc_ids {
            query = query.filter(Filter::any_of(segment::REQUEST_DOC_ID, ids));
        }
        if !self.sources.is_empty() {
            query = query.filter(Filter::any(self.sources.iter().map(
                |source| match source {
                    OutputSource::ProviderTurn { turn_index, .. } => {
                        Filter::json_eq(SOURCE_TURN, *turn_index)
                    }
                    OutputSource::ToolCall { tool_call_doc_id }
                    | OutputSource::ToolEffectArguments { tool_call_doc_id } => {
                        Filter::json_eq(SOURCE_TOOL, tool_call_doc_id)
                    }
                    OutputSource::Authored { key } => Filter::json_eq(SOURCE_KEY, key),
                },
            )));
        }
        if self.provider_live_only {
            query = query.filter(
                Filter::json_eq(SOURCE_KIND, "provider_turn")
                    .and(Filter::json_like(SOURCE_SCOPE, "inference.%")),
            );
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ToolOrder {
    Sequence,
    Started,
    Completed,
}

#[derive(Debug, Clone)]
pub struct ToolCallRows {
    pub scope: Scope,
    pub document_ids: Option<Vec<String>>,
    pub session_id: Option<String>,
    pub request_doc_ids: Option<Vec<String>>,
    pub sequence: SequenceRange,
    pub lifecycle_states: Option<Vec<String>>,
    pub await_mode: Option<String>,
    pub started_only: bool,
    pub completed_only: bool,
    pub order: ToolOrder,
    pub direction: Direction,
    pub limit: Option<u64>,
}

impl ToolCallRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            document_ids: None,
            session_id: None,
            request_doc_ids: None,
            sequence: SequenceRange::All,
            lifecycle_states: None,
            await_mode: None,
            started_only: false,
            completed_only: false,
            order: ToolOrder::Sequence,
            direction: Direction::Asc,
            limit: None,
        }
    }

    pub fn for_request(scope: SessionScope, request_doc_id: impl Into<String>) -> Self {
        let mut view = Self::new(Scope::Session(scope));
        view.request_doc_ids = Some(vec![request_doc_id.into()]);
        view.order = ToolOrder::Started;
        view
    }

    pub fn running(scope: Scope) -> Self {
        let mut view = Self::new(scope);
        view.lifecycle_states = Some(vec!["running".to_owned()]);
        view
    }

    pub fn background(scope: Scope) -> Self {
        let mut view = Self::new(scope);
        view.await_mode = Some("background".to_owned());
        view.lifecycle_states = Some(vec!["pending".to_owned(), "running".to_owned()]);
        view
    }
}

impl View for ToolCallRows {
    type Collection = tool::Doc;
    type Row = gents_protocol::row::AgentToolCallRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(tool::DOC_ID)
                .field(tool::TOOL_CALL_KEY)
                .field(tool::NODE_DID)
                .field(tool::SESSION_ID)
                .field(tool::REQUEST_ID)
                .field(tool::REQUEST_DOC_ID)
                .field(tool::REQUESTER_DID)
                .field(tool::MESSAGE_SEQUENCE)
                .field(tool::TOOL_NAME)
                .field(tool::TOOL_CALL_ID)
                .field(tool::STATUS)
                .field(tool::LIFECYCLE_STATE)
                .field(tool::AWAIT_MODE)
                .field(tool::STUCK_SINCE)
                .field(tool::DEADLINE_AT)
                .field(tool::CANCEL_CAUSE)
                .field(tool::STARTED_AT)
                .field(tool::COMPLETED_AT)
                .field(tool::SELECTED_SERVICE_ID)
                .field(tool::SELECTED_TOOL_NAME)
                .field(tool::TOOL_FAILURE_CLASS)
                .field(tool::DENIAL_REASON)
                .field(tool::DENIED_ARGV)
                .field(tool::DENIED_COMMAND)
                .field(tool::DENIED_ARGUMENT)
                .field(tool::DENIED_SUBCOMMAND)
                .field(tool::DENIED_PREFIX)
                .field(tool::POLICY_MODE)
                .field(tool::POLICY_NETWORK)
                .field(tool::LATENCY_MS)
                .field(tool::JOB_PIN)
                .field(tool::JOB_PROGRESS)
                .field(tool::SPAWNED_BY_TOOL_CALL_DOC_ID)
                .field(tool::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
                .field(tool::TERMINAL_OUTPUT),
        )
        .filter(
            self.scope
                .filter(tool::NODE_DID, tool::SESSION_ID, tool::REQUESTER_DID),
        );
        query = match self.order {
            ToolOrder::Sequence => query.order_by(tool::MESSAGE_SEQUENCE, self.direction),
            ToolOrder::Started => query.order_by(tool::STARTED_AT, self.direction),
            ToolOrder::Completed => query.order_by(tool::COMPLETED_AT, self.direction),
        }
        .order_by(tool::TOOL_CALL_KEY, self.direction);
        if let Some(ids) = &self.document_ids {
            query = query.filter(Filter::any_of(tool::DOC_ID, ids));
        }
        if let Some(id) = &self.session_id {
            query = query.filter(Filter::eq(tool::SESSION_ID, id));
        }
        if let Some(ids) = &self.request_doc_ids {
            query = query.filter(Filter::any_of(tool::REQUEST_DOC_ID, ids));
        }
        if let Some(filter) = self.sequence.filter(tool::MESSAGE_SEQUENCE) {
            query = query.filter(filter);
        }
        if let Some(states) = &self.lifecycle_states {
            query = query.filter(Filter::any_of(tool::LIFECYCLE_STATE, states));
        }
        if let Some(mode) = &self.await_mode {
            query = query.filter(Filter::eq(tool::AWAIT_MODE, mode));
        }
        if self.started_only {
            query = query.filter(Filter::not_null(tool::STARTED_AT));
        }
        if self.completed_only {
            query = query.filter(Filter::not_null(tool::COMPLETED_AT));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolSequenceRow {
    pub message_sequence: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ToolSequences(pub ToolCallRows);

impl View for ToolSequences {
    type Collection = tool::Doc;
    type Row = ToolSequenceRow;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        self.0
            .query(reader_did)
            .select_fields(Selection::new().field(tool::MESSAGE_SEQUENCE))
    }
}

#[derive(Debug, Clone)]
pub struct CompactionRows {
    pub scope: Scope,
    pub document_ids: Option<Vec<String>>,
    pub compaction_keys: Option<Vec<String>>,
    pub session_ids: Option<Vec<String>>,
    pub request_doc_ids: Option<Vec<String>>,
    pub limit: Option<u64>,
}

impl CompactionRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            document_ids: None,
            compaction_keys: None,
            session_ids: None,
            request_doc_ids: None,
            limit: None,
        }
    }
}

impl View for CompactionRows {
    type Collection = compaction::Doc;
    type Row = gents_protocol::row::CompactionEntryRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(compaction::COMPACTION_KEY)
                .field(compaction::SESSION_ID)
                .field(compaction::REQUESTER_DID)
                .field(compaction::REQUEST_DOC_ID)
                .field(compaction::SEQUENCE)
                .field(compaction::SUMMARY)
                .field(compaction::FILES_READ)
                .field(compaction::FILES_MODIFIED)
                .field(compaction::MESSAGES_COMPACTED)
                .field(compaction::COMPACTED_THROUGH_SEQUENCE)
                .field(compaction::ORIGINAL_TOKENS)
                .field(compaction::COMPACTED_TOKENS)
                .field(compaction::CREATED_AT),
        )
        .filter(self.scope.filter(
            compaction::NODE_DID,
            compaction::SESSION_ID,
            compaction::REQUESTER_DID,
        ))
        .order_by(compaction::SEQUENCE, Direction::Asc)
        .order_by(compaction::COMPACTION_KEY, Direction::Asc);
        if let Some(ids) = &self.document_ids {
            query = query.filter(Filter::any_of(compaction::DOC_ID, ids));
        }
        if let Some(keys) = &self.compaction_keys {
            query = query.filter(Filter::any_of(compaction::COMPACTION_KEY, keys));
        }
        if let Some(ids) = &self.session_ids {
            query = query.filter(Filter::any_of(compaction::SESSION_ID, ids));
        }
        if let Some(ids) = &self.request_doc_ids {
            query = query.filter(Filter::any_of(compaction::REQUEST_DOC_ID, ids));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
