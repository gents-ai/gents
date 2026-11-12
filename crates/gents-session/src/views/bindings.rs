use gents_protocol::document_query::{
    fields::agent_request, session_scope, Direction, Filter, Query, Selection,
};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::views::View;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RequestBindingRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_id: String,
}

pub struct SessionRequestBindings {
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
    physical: Option<Vec<String>>,
    after: Option<String>,
    limit: Option<u64>,
}

impl SessionRequestBindings {
    pub fn new(
        node_did: impl Into<String>,
        requester_did: Option<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            node_did: node_did.into(),
            session_id: session_id.into(),
            requester_did,
            physical: None,
            after: None,
            limit: None,
        }
    }

    pub fn physical(mut self, ids: Vec<String>) -> Self {
        self.physical = Some(ids);
        self
    }

    pub fn after(mut self, doc_id: impl Into<String>) -> Self {
        self.after = Some(doc_id.into());
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

impl View for SessionRequestBindings {
    type Collection = agent_request::Doc;
    type Row = RequestBindingRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(agent_request::DOC_ID)
                .field(agent_request::REQUEST_ID),
        )
        .filter(session_scope(
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        ))
        .filter(Filter::eq(
            agent_request::PURPOSE,
            RequestPurpose::Normal.as_str(),
        ))
        .order_by(agent_request::DOC_ID, Direction::Asc);
        if let Some(ids) = &self.physical {
            query = query.filter(Filter::any_of(agent_request::DOC_ID, ids));
        }
        if let Some(after) = &self.after {
            query = query.filter(Filter::gt(agent_request::DOC_ID, after));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
