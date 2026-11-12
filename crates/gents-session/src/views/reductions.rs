use gents_protocol::document_query::{
    fields::provider_context_reduction as reduction, Direction, Filter, Query, Selection,
};
use gents_protocol::views::View;

use super::transcript::Scope;

#[derive(Debug, Clone)]
pub struct ReductionRows {
    pub scope: Scope,
    pub reduction_keys: Option<Vec<String>>,
    pub request_doc_ids: Option<Vec<String>>,
    pub limit: Option<u64>,
}

impl ReductionRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            reduction_keys: None,
            request_doc_ids: None,
            limit: None,
        }
    }
}

impl View for ReductionRows {
    type Collection = reduction::Doc;
    type Row = crate::session::provider_context_reduction::ProviderContextReduction;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(reduction::DOC_ID)
                .field(reduction::REDUCTION_KEY)
                .field(reduction::NODE_DID)
                .field(reduction::REQUESTER_DID)
                .field(reduction::SESSION_ID)
                .field(reduction::REQUEST_ID)
                .field(reduction::REQUEST_DOC_ID)
                .field(reduction::REQUEST_COMMIT_CID)
                .field(reduction::REDUCTION_INDEX)
                .field(reduction::TURN_INDEX)
                .field(reduction::PARENT_REDUCTION_KEY)
                .field(reduction::PRODUCER_CALL_ID)
                .field(reduction::PRODUCER_CALL_SEQ)
                .field(reduction::SOURCE_BOUNDARY_JSON)
                .field(reduction::COMPACTED_PREFIX_JSON)
                .field(reduction::RETAINED_SUFFIX_JSON)
                .field(reduction::PAIR_CLOSED)
                .field(reduction::CHECKPOINT_MESSAGES_JSON)
                .field(reduction::REPLAY_ASSOCIATIONS_JSON)
                .field(reduction::SUMMARY)
                .field(reduction::MESSAGES_COMPACTED)
                .field(reduction::ORIGINAL_TOKENS)
                .field(reduction::COMPACTED_TOKENS)
                .field(reduction::CREATED_AT),
        )
        .filter(self.scope.filter(
            reduction::NODE_DID,
            reduction::SESSION_ID,
            reduction::REQUESTER_DID,
        ))
        .order_by(reduction::CREATED_AT, Direction::Asc)
        .order_by(reduction::REDUCTION_KEY, Direction::Asc);
        if let Some(keys) = &self.reduction_keys {
            query = query.filter(Filter::any_of(reduction::REDUCTION_KEY, keys));
        }
        if let Some(ids) = &self.request_doc_ids {
            query = query.filter(Filter::any_of(reduction::REQUEST_DOC_ID, ids));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
