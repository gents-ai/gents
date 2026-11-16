use gents_protocol::document_query::{fields::inference_call, Direction, Filter, Query, Selection};
use gents_protocol::usage::TokenUsageColumns;
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InferenceCallRow {
    pub call_id: String,
    pub call_seq: Option<i64>,
    pub node_did: String,
    pub agent_did: Option<String>,
    pub request_id: Option<String>,
    pub request_doc_id: Option<String>,
    pub backend_id: Option<String>,
    pub call_state: String,
    pub call_kind: Option<String>,
    pub model_name: Option<String>,
    pub failure_reason: Option<String>,
    pub queued_at: Option<String>,
    pub queue_depth_at_enqueue: Option<i64>,
    pub context_accounting_json: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    #[serde(flatten)]
    pub usage: TokenUsageColumns,
}

#[derive(Debug, Clone, Copy)]
pub enum ActivityEdge {
    Started,
    Ended,
}

enum CallFilter {
    All,
    Active,
    CompletedSince(String),
    Activity {
        request_doc_id: String,
        edge: ActivityEdge,
    },
    Backend(String),
    Contexts(Vec<String>),
    ContextCall(String),
}

pub struct InferenceCalls {
    node_did: String,
    filter: CallFilter,
    call_id: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl InferenceCalls {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            filter: CallFilter::All,
            call_id: None,
            limit: None,
            offset: None,
        }
    }

    pub fn active(mut self) -> Self {
        self.filter = CallFilter::Active;
        self
    }

    pub fn completed_since(mut self, since: impl Into<String>) -> Self {
        self.filter = CallFilter::CompletedSince(since.into());
        self
    }

    pub fn latest_activity(
        mut self,
        request_doc_id: impl Into<String>,
        edge: ActivityEdge,
    ) -> Self {
        self.filter = CallFilter::Activity {
            request_doc_id: request_doc_id.into(),
            edge,
        };
        self.limit = Some(1);
        self
    }

    pub fn recent_backend(mut self, backend_id: impl Into<String>) -> Self {
        self.filter = CallFilter::Backend(backend_id.into());
        self
    }

    pub fn contexts(mut self, request_doc_ids: Vec<String>) -> Self {
        self.filter = CallFilter::Contexts(request_doc_ids);
        self
    }

    pub fn context_call(mut self, request_id: impl Into<String>) -> Self {
        self.filter = CallFilter::ContextCall(request_id.into());
        self.limit = Some(2);
        self
    }

    pub fn call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
}

impl View for InferenceCalls {
    type Collection = inference_call::Doc;
    type Row = InferenceCallRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let selection = Selection::new()
            .field(inference_call::CALL_ID)
            .field(inference_call::CALL_SEQ)
            .field(inference_call::NODE_DID)
            .field(inference_call::AGENT_DID)
            .field(inference_call::REQUEST_ID)
            .field(inference_call::REQUEST_DOC_ID)
            .field(inference_call::BACKEND_ID)
            .field(inference_call::CALL_STATE)
            .field(inference_call::CALL_KIND)
            .field(inference_call::MODEL_NAME)
            .field(inference_call::FAILURE_REASON)
            .field(inference_call::QUEUED_AT)
            .field(inference_call::QUEUE_DEPTH_AT_ENQUEUE)
            .field(inference_call::CONTEXT_ACCOUNTING_JSON)
            .field(inference_call::STARTED_AT)
            .field(inference_call::ENDED_AT)
            .field(inference_call::PROMPT_TOKENS)
            .field(inference_call::COMPLETION_TOKENS)
            .field(inference_call::CACHED_INPUT_TOKENS)
            .field(inference_call::CACHE_CREATION_INPUT_TOKENS)
            .field(inference_call::REASONING_OUTPUT_TOKENS)
            .field(inference_call::PROVIDER_TOTAL_TOKENS)
            .field(inference_call::USAGE_COMPLETENESS);
        let mut query =
            Query::select(selection).filter(Filter::eq(inference_call::NODE_DID, &self.node_did));
        query = match &self.filter {
            CallFilter::All => query.order_by(inference_call::CALL_ID, Direction::Asc),
            CallFilter::Active => query
                .filter(Filter::any_of(
                    inference_call::CALL_STATE,
                    ["queued", "running"],
                ))
                .order_by(inference_call::CALL_ID, Direction::Asc),
            CallFilter::CompletedSince(since) => query
                .filter(Filter::any_of(
                    inference_call::CALL_STATE,
                    ["completed", "failed", "cancelled"],
                ))
                .filter(Filter::any_of(
                    inference_call::CALL_KIND,
                    ["inference", "compaction", "scheduled", "oneoff"],
                ))
                .filter(Filter::ge(inference_call::ENDED_AT, since))
                .order_by(inference_call::ENDED_AT, Direction::Desc)
                .order_by(inference_call::CALL_ID, Direction::Desc),
            CallFilter::Activity {
                request_doc_id,
                edge,
            } => {
                let field = match edge {
                    ActivityEdge::Started => inference_call::STARTED_AT,
                    ActivityEdge::Ended => inference_call::ENDED_AT,
                };
                query
                    .filter(Filter::eq(inference_call::REQUEST_DOC_ID, request_doc_id))
                    .filter(Filter::not_null(field))
                    .order_by(field, Direction::Desc)
                    .order_by(inference_call::CALL_ID, Direction::Desc)
            }
            CallFilter::Backend(backend_id) => query
                .filter(Filter::eq(inference_call::BACKEND_ID, backend_id))
                .order_by(inference_call::QUEUED_AT, Direction::Desc)
                .order_by(inference_call::CALL_ID, Direction::Desc),
            CallFilter::Contexts(request_doc_ids) => query
                .filter(Filter::eq(inference_call::CALL_KIND, "inference"))
                .filter(Filter::any_of(
                    inference_call::REQUEST_DOC_ID,
                    request_doc_ids,
                ))
                .order_by(inference_call::QUEUED_AT, Direction::Asc)
                .order_by(inference_call::REQUEST_DOC_ID, Direction::Asc)
                .order_by(inference_call::CALL_SEQ, Direction::Asc)
                .order_by(inference_call::CALL_ID, Direction::Asc),
            CallFilter::ContextCall(request_id) => query
                .filter(Filter::eq(inference_call::CALL_KIND, "inference"))
                .filter(Filter::eq(inference_call::REQUEST_ID, request_id))
                .order_by(inference_call::CALL_ID, Direction::Asc),
        };
        if let Some(call_id) = &self.call_id {
            query = query.filter(Filter::eq(inference_call::CALL_ID, call_id));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        if let Some(offset) = self.offset {
            query = query.offset(offset);
        }
        query
    }
}
