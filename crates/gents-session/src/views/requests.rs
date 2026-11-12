use gents_protocol::document_query::{
    fields::agent_request as request, Direction, Filter, Query, Selection,
};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::views::View;

use super::transcript::Scope;

#[derive(Debug, Clone, Copy, Default)]
pub enum RequestOrder {
    #[default]
    Newest,
    Oldest,
    Physical,
    /// DefraDB's planner requires the node/trigger/purpose equality prefix for
    /// this index. Cutoff twins need a separate physical-ID boundary read;
    /// an implicit physical suffix cannot be combined with this ordering.
    TriggerRecent,
}

#[derive(Debug, Clone)]
pub struct RequestRows {
    scope: Scope,
    document_ids: Option<Vec<String>>,
    request_ids: Option<Vec<String>>,
    retry_parent_ids: Option<Vec<String>>,
    states: Option<Vec<RequestLifecycleState>>,
    public_only: bool,
    trigger_id: Option<String>,
    created_at: Option<Option<String>>,
    session_id: Option<Option<String>>,
    seeds: Option<Vec<String>>,
    order: RequestOrder,
    limit: Option<u64>,
}

impl RequestRows {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            document_ids: None,
            request_ids: None,
            retry_parent_ids: None,
            states: None,
            public_only: false,
            trigger_id: None,
            created_at: None,
            session_id: None,
            seeds: None,
            order: RequestOrder::default(),
            limit: None,
        }
    }

    pub fn doc_ids(mut self, document_ids: Vec<String>) -> Self {
        self.document_ids = Some(document_ids);
        self
    }

    pub fn request_ids(mut self, request_ids: Vec<String>) -> Self {
        self.request_ids = Some(request_ids);
        self
    }

    pub fn retry_parent_ids(mut self, request_ids: Vec<String>) -> Self {
        self.retry_parent_ids = Some(request_ids);
        self
    }

    pub fn states(mut self, states: Vec<RequestLifecycleState>) -> Self {
        self.states = Some(states);
        self
    }

    pub fn nonterminal(self) -> Self {
        self.states(
            RequestLifecycleState::ALL
                .into_iter()
                .filter(|state| !state.is_terminal())
                .collect(),
        )
    }

    pub fn active_runtime(self) -> Self {
        self.states(
            RequestLifecycleState::ALL
                .into_iter()
                .filter(|state| state.is_active_runtime())
                .collect(),
        )
    }

    pub fn processing(self) -> Self {
        self.states(vec![RequestLifecycleState::Processing])
    }

    pub fn public(mut self) -> Self {
        self.public_only = true;
        self
    }

    pub fn trigger(mut self, trigger_id: impl Into<String>) -> Self {
        self.trigger_id = Some(trigger_id.into());
        self
    }

    pub fn created_at(mut self, created_at: Option<String>) -> Self {
        self.created_at = Some(created_at);
        self
    }

    pub fn session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn seeds(mut self, page_document_ids: Vec<String>) -> Self {
        self.seeds = Some(page_document_ids);
        self
    }

    pub fn order(mut self, order: RequestOrder) -> Self {
        self.order = order;
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn trigger_candidates(scope: Scope, trigger_id: impl Into<String>) -> Self {
        Self::new(scope)
            .trigger(trigger_id)
            .public()
            .order(RequestOrder::TriggerRecent)
            .limit(8)
    }
}

impl View for RequestRows {
    type Collection = request::Doc;
    type Row = AgentRequestRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query =
            Query::select(Selection::all().field(request::DOC_ID)).filter(self.scope.filter(
                request::NODE_DID,
                request::SESSION_ID,
                request::REQUESTER_DID,
            ));
        if let Some(ids) = &self.document_ids {
            query = query.filter(Filter::any_of(request::DOC_ID, ids));
        }
        if let Some(ids) = &self.request_ids {
            query = query.filter(Filter::any_of(request::REQUEST_ID, ids));
        }
        if let Some(ids) = &self.retry_parent_ids {
            query = query.filter(Filter::any_of(request::RETRY_PARENT_REQUEST, ids));
        }
        if let Some(states) = &self.states {
            query = query.filter(Filter::any_of(
                request::LIFECYCLE_STATE,
                states.iter().map(|state| state.as_str()),
            ));
        }
        if self.public_only {
            query = query.filter(Filter::eq(
                request::PURPOSE,
                RequestPurpose::Normal.as_str(),
            ));
        }
        if let Some(trigger) = &self.trigger_id {
            query = query.filter(Filter::eq(request::CAUSED_BY_TRIGGER_ID, trigger));
        }
        if let Some(created_at) = &self.created_at {
            query = query.filter(match created_at {
                Some(created_at) => Filter::eq(request::CREATED_AT, created_at),
                None => Filter::is_null(request::CREATED_AT),
            });
        }
        if let Some(session_id) = &self.session_id {
            query = query.filter(match session_id {
                Some(session_id) => Filter::eq(request::SESSION_ID, session_id),
                None => Filter::is_null(request::SESSION_ID),
            });
        }
        if let Some(page) = &self.seeds {
            let mut alternatives = vec![
                Filter::ne(
                    request::LIFECYCLE_STATE,
                    RequestLifecycleState::Completed.as_str(),
                ),
                Filter::is_null(request::LIFECYCLE_STATE),
                Filter::not_null(request::RETRY_PARENT_REQUEST),
                Filter::not_null(request::RETRY_PARENT_REQUEST_DOC_ID),
                Filter::not_null(request::SUPERSEDED_BY_REQUEST_DOC_ID),
            ];
            if !page.is_empty() {
                alternatives.push(Filter::any_of(request::DOC_ID, page));
            }
            query = query.filter(Filter::any(alternatives));
        }
        query = match self.order {
            RequestOrder::Newest => query
                .order_by(request::CREATED_AT, Direction::Desc)
                .order_by(request::REQUEST_ID, Direction::Desc)
                .order_by(request::DOC_ID, Direction::Desc),
            RequestOrder::Oldest => query
                .order_by(request::CREATED_AT, Direction::Asc)
                .order_by(request::REQUEST_ID, Direction::Asc)
                .order_by(request::DOC_ID, Direction::Asc),
            RequestOrder::Physical => query.order_by(request::DOC_ID, Direction::Asc),
            RequestOrder::TriggerRecent => query
                .order_by(request::NODE_DID, Direction::Asc)
                .order_by(request::CAUSED_BY_TRIGGER_ID, Direction::Asc)
                .order_by(request::PURPOSE, Direction::Asc)
                .order_by(request::CREATED_AT, Direction::Desc)
                .order_by(request::REQUEST_ID, Direction::Desc)
                .order_by(request::SESSION_ID, Direction::Asc),
        };
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

#[derive(Debug, Clone)]
pub struct CausedRequests {
    parent_document_ids: Vec<String>,
}

impl CausedRequests {
    pub fn new(parent_document_ids: Vec<String>) -> Self {
        Self {
            parent_document_ids,
        }
    }
}

impl View for CausedRequests {
    type Collection = request::Doc;
    type Row = AgentRequestRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(Selection::all().field(request::DOC_ID))
            .filter(Filter::any_of(
                request::CAUSED_BY_PARENT_REQUEST_DOC_ID,
                &self.parent_document_ids,
            ))
            .filter(Filter::not_null(request::CAUSED_BY_PARENT_TOOL_CALL_DOC_ID))
            .filter(Filter::eq(
                request::PURPOSE,
                RequestPurpose::Normal.as_str(),
            ))
            .order_by(request::CREATED_AT, Direction::Asc)
            .order_by(request::REQUEST_ID, Direction::Asc)
            .order_by(request::DOC_ID, Direction::Asc)
    }
}
