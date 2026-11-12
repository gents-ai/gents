use gents_protocol::document_query::{Direction, Filter, Query, Selection};
use gents_protocol::views::View;
use gents_schemas::fields::{node_readiness, node_runtime};

pub struct NodeRuntimeList<'a> {
    node_did: &'a str,
    doc_ids: Option<Vec<String>>,
    limit: Option<u64>,
}

impl<'a> NodeRuntimeList<'a> {
    pub fn new(node_did: &'a str) -> Self {
        Self {
            node_did,
            doc_ids: None,
            limit: None,
        }
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }
}

impl View for NodeRuntimeList<'_> {
    type Collection = node_runtime::Doc;
    type Row = gents_protocol::row::NodeRuntimeRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(node_runtime::NODE_DID)
                .field(node_runtime::RECONCILE_PHASE)
                .field(node_runtime::AGENT_EXECUTOR_CAPACITY)
                .field(node_runtime::AGENT_EXECUTOR_QUEUE_DEPTH)
                .field(node_runtime::AGENT_EXECUTOR_STATUS_JSON)
                .field(node_runtime::LAST_RECONCILE_RESULT)
                .field(node_runtime::LAST_RECONCILE_ERROR)
                .field(node_runtime::LAST_RECONCILE_COMPLETED_AT)
                .field(node_runtime::UPDATED_AT),
        )
        .filter(Filter::eq(node_runtime::NODE_DID, self.node_did))
        .order_by(node_runtime::NODE_DID, Direction::Asc);
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(node_runtime::DOC_ID, doc_ids));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

pub struct NodeReadinessList<'a> {
    node_did: &'a str,
    doc_ids: Option<Vec<String>>,
    limit: Option<u64>,
}

impl<'a> NodeReadinessList<'a> {
    pub fn new(node_did: &'a str) -> Self {
        Self {
            node_did,
            doc_ids: None,
            limit: None,
        }
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }
}

impl View for NodeReadinessList<'_> {
    type Collection = node_readiness::Doc;
    type Row = gents_protocol::row::NodeReadinessRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(node_readiness::NODE_DID)
                .field(node_readiness::SNAPSHOT_JSON)
                .field(node_readiness::UPDATED_AT),
        )
        .filter(Filter::eq(node_readiness::NODE_DID, self.node_did))
        .order_by(node_readiness::UPDATED_AT, Direction::Desc)
        .order_by(node_readiness::DOC_ID, Direction::Asc);
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(node_readiness::DOC_ID, doc_ids));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
