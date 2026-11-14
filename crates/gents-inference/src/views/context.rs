use gents_protocol::document_query::{
    fields::inference_call, kind, Direction, Field, Filter, Query, Selection,
};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextCallRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_doc_id: Option<String>,
    pub request_id: Option<String>,
    pub call_id: Option<String>,
    pub call_seq: Option<i64>,
    pub queued_at: Option<String>,
    pub context_accounting_json: Option<String>,
}

pub fn retain_latest(rows: &mut Vec<ContextCallRow>, limit: usize) {
    rows.sort_by(|left, right| {
        right
            .queued_at
            .cmp(&left.queued_at)
            .then_with(|| right.call_seq.cmp(&left.call_seq))
            .then_with(|| right.call_id.cmp(&left.call_id))
            .then_with(|| right.doc_id.cmp(&left.doc_id))
    });
    rows.truncate(limit);
}

#[derive(Debug, Clone)]
pub struct ContextCursor {
    pub queued_at: Option<String>,
    pub call_seq: Option<i64>,
    pub call_id: Option<String>,
    pub doc_id: String,
}

impl From<&ContextCallRow> for ContextCursor {
    fn from(row: &ContextCallRow) -> Self {
        Self {
            queued_at: row.queued_at.clone(),
            call_seq: row.call_seq,
            call_id: row.call_id.clone(),
            doc_id: row.doc_id.clone(),
        }
    }
}

enum Scope {
    Indexed(String),
    Retained(Option<String>),
}

pub struct ContextCalls {
    node_did: String,
    scope: Scope,
    requests: Option<Vec<String>>,
    after: Option<ContextCursor>,
    limit: Option<u64>,
}

impl ContextCalls {
    pub fn indexed(node_did: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            scope: Scope::Indexed(session_id.into()),
            requests: None,
            after: None,
            limit: None,
        }
    }

    pub fn retained(node_did: impl Into<String>, queued_at_least: Option<String>) -> Self {
        Self {
            node_did: node_did.into(),
            scope: Scope::Retained(queued_at_least),
            requests: None,
            after: None,
            limit: None,
        }
    }

    pub fn for_requests(mut self, requests: Vec<String>) -> Self {
        self.requests = Some(requests);
        self
    }

    pub fn after(mut self, cursor: ContextCursor) -> Self {
        self.after = Some(cursor);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

impl View for ContextCalls {
    type Collection = inference_call::Doc;
    type Row = ContextCallRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(inference_call::DOC_ID)
                .field(inference_call::REQUEST_DOC_ID)
                .field(inference_call::REQUEST_ID)
                .field(inference_call::CALL_ID)
                .field(inference_call::CALL_SEQ)
                .field(inference_call::QUEUED_AT)
                .field(inference_call::CONTEXT_ACCOUNTING_JSON),
        )
        .filter(Filter::eq(inference_call::NODE_DID, &self.node_did))
        .filter(Filter::eq(inference_call::CALL_KIND, "inference"))
        .order_by(inference_call::QUEUED_AT, Direction::Desc)
        .order_by(inference_call::CALL_SEQ, Direction::Desc)
        .order_by(inference_call::CALL_ID, Direction::Desc)
        .order_by(inference_call::DOC_ID, Direction::Desc);
        query = match &self.scope {
            Scope::Indexed(session_id) => {
                query.filter(Filter::eq(inference_call::SESSION_ID, session_id))
            }
            Scope::Retained(lower_bound) => {
                let query = query.filter(Filter::any([
                    Filter::is_null(inference_call::SESSION_ID),
                    Filter::eq(inference_call::SESSION_ID, ""),
                ]));
                if let Some(lower_bound) = lower_bound {
                    query.filter(Filter::ge(inference_call::QUEUED_AT, lower_bound))
                } else {
                    query
                }
            }
        };
        if let Some(requests) = &self.requests {
            query = query.filter(Filter::any_of(inference_call::REQUEST_DOC_ID, requests));
        }
        if let Some(cursor) = &self.after {
            query = query.filter(descending_cursor(cursor));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

fn text_equal(
    field: Field<inference_call::Doc, kind::Str>,
    value: Option<&str>,
) -> Filter<inference_call::Doc> {
    match value {
        Some(value) => Filter::eq(field, value),
        None => Filter::is_null(field),
    }
}

fn descending_cursor(cursor: &ContextCursor) -> Filter<inference_call::Doc> {
    let queued_equal = text_equal(inference_call::QUEUED_AT, cursor.queued_at.as_deref());
    let sequence_equal = match cursor.call_seq {
        Some(value) => Filter::eq(inference_call::CALL_SEQ, value),
        None => Filter::is_null(inference_call::CALL_SEQ),
    };
    let call_equal = text_equal(inference_call::CALL_ID, cursor.call_id.as_deref());
    let mut branches = Vec::new();
    if let Some(value) = &cursor.queued_at {
        branches.push(Filter::any([
            Filter::lt(inference_call::QUEUED_AT, value),
            Filter::is_null(inference_call::QUEUED_AT),
        ]));
    }
    if let Some(value) = cursor.call_seq {
        branches.push(queued_equal.clone().and(Filter::any([
            Filter::lt(inference_call::CALL_SEQ, value),
            Filter::is_null(inference_call::CALL_SEQ),
        ])));
    }
    if let Some(value) = &cursor.call_id {
        branches.push(Filter::all([
            queued_equal.clone(),
            sequence_equal.clone(),
            Filter::any([
                Filter::lt(inference_call::CALL_ID, value),
                Filter::is_null(inference_call::CALL_ID),
            ]),
        ]));
    }
    branches.push(Filter::all([
        queued_equal,
        sequence_equal,
        call_equal,
        Filter::lt(inference_call::DOC_ID, &cursor.doc_id),
    ]));
    Filter::any(branches)
}
