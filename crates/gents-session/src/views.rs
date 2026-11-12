pub mod bindings;
pub mod reductions;
pub mod requests;
pub mod transcript;

use gents_protocol::document_query::{
    fields::agent_session as session, Direction, Filter, Query, Selection,
};
use gents_protocol::operations::session::SessionListInput;
use gents_protocol::session::{decode_session_row, AgentSession};
use gents_protocol::views::View;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub session: AgentSession,
}

impl<'de> Deserialize<'de> for SessionRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let row = decode_session_row(&value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            doc_id: row.doc_id,
            session: row.session,
        })
    }
}

fn selection() -> Selection<session::Doc> {
    Selection::new()
        .field(session::DOC_ID)
        .field(session::SESSION_ID)
        .field(session::NODE_DID)
        .field(session::REQUESTER_DID)
        .field(session::AGENT_DID)
        .field(session::CREATED_AT)
        .field(session::CLOSED_AT)
        .field(session::STOPPED_AT)
        .field(session::ARCHIVED_AT)
        .field(session::ARCHIVE_REVISION)
        .field(session::TITLE)
        .field(session::PROVENANCE)
        .field(session::OBSERVATION)
        .field(session::TAGS)
}

fn scoped(input: &SessionListInput) -> Query<session::Doc> {
    let mut query = Query::select(selection());
    if let Some(node) = &input.node_did {
        query = query.filter(Filter::eq(session::NODE_DID, node));
    }
    if let Some(requester) = &input.requester_did {
        query = query.filter(Filter::eq(session::REQUESTER_DID, requester));
    }
    if let Some(id) = &input.session_id {
        query = query.filter(Filter::eq(session::SESSION_ID, id));
    }
    if !input.include_archived {
        query = query.filter(Filter::is_null(session::ARCHIVED_AT));
    }
    query
}

#[derive(Debug, Clone, Default)]
pub struct SessionList {
    pub input: SessionListInput,
}

impl View for SessionList {
    type Collection = session::Doc;
    type Row = SessionRow;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        SessionPage {
            input: self.input.clone(),
            ..Default::default()
        }
        .query(reader_did)
    }
}

#[derive(Debug, Clone)]
pub struct SessionPosition {
    pub created_at: String,
    pub session_id: String,
    pub node_did: Option<String>,
    pub inclusive: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum SessionState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Default)]
pub struct SessionPage {
    pub input: SessionListInput,
    pub agent_did: Option<String>,
    pub exclude_session_id: Option<String>,
    pub doc_id: Option<String>,
    pub doc_ids: Option<Vec<String>>,
    pub state: Option<SessionState>,
    pub tag: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub after: Option<SessionPosition>,
    pub limit: Option<u64>,
}

impl View for SessionPage {
    type Collection = session::Doc;
    type Row = SessionRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = ordered(scoped(&self.input));
        if let Some(agent) = &self.agent_did {
            query = query.filter(Filter::eq(session::AGENT_DID, agent));
        }
        if let Some(id) = &self.exclude_session_id {
            query = query.filter(Filter::ne(session::SESSION_ID, id));
        }
        if let Some(id) = &self.doc_id {
            query = query.filter(Filter::eq(session::DOC_ID, id));
        }
        if let Some(ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(session::DOC_ID, ids));
        }
        if let Some(state) = self.state {
            query = query.filter(match state {
                SessionState::Open => Filter::is_null(session::CLOSED_AT),
                SessionState::Closed => Filter::not_null(session::CLOSED_AT),
            });
        }
        if let Some(tag) = &self.tag {
            query = query.filter(Filter::contains(session::TAGS, tag));
        }
        if let Some(time) = &self.created_after {
            query = query.filter(Filter::ge(session::CREATED_AT, time));
        }
        if let Some(time) = &self.created_before {
            query = query.filter(Filter::lt(session::CREATED_AT, time));
        }
        if let Some(after) = &self.after {
            let id = if after.inclusive {
                Filter::le(session::SESSION_ID, &after.session_id)
            } else {
                Filter::lt(session::SESSION_ID, &after.session_id)
            };
            let mut branches = vec![
                Filter::lt(session::CREATED_AT, &after.created_at),
                Filter::eq(session::CREATED_AT, &after.created_at).and(id),
            ];
            if let Some(node) = &after.node_did {
                branches.push(
                    Filter::eq(session::CREATED_AT, &after.created_at)
                        .and(Filter::eq(session::SESSION_ID, &after.session_id))
                        .and(Filter::lt(session::NODE_DID, node)),
                );
            }
            query = query.filter(Filter::any(branches));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestSessionRow {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl RequestSessionRow {
    pub fn scope(self) -> gents_protocol::session::SessionScope {
        gents_protocol::session::SessionScope {
            node_did: self.node_did,
            session_id: self.session_id,
            requester_did: self.requester_did,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionRequestScopes {
    pub scopes: Vec<gents_protocol::session::SessionScope>,
}

impl View for SessionRequestScopes {
    type Collection = gents_protocol::document_query::fields::agent_request::Doc;
    type Row = RequestSessionRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use gents_protocol::document_query::{fields::agent_request as request, session_scope};
        Query::select(
            Selection::new()
                .field(request::NODE_DID)
                .field(request::SESSION_ID)
                .field(request::REQUESTER_DID),
        )
        .filter(Filter::eq(
            request::PURPOSE,
            gents_protocol::request_admission::RequestPurpose::PUBLIC.as_str(),
        ))
        .filter(Filter::any(self.scopes.iter().map(|scope| {
            session_scope(
                &scope.node_did,
                &scope.session_id,
                scope.requester_did.as_deref(),
            )
        })))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionCount {
    pub page: SessionPage,
}

impl View for SessionCount {
    type Collection = session::Doc;
    type Row = u64;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        self.page.query(reader_did).counted()
    }
}

fn ordered(query: Query<session::Doc>) -> Query<session::Doc> {
    query
        .order_by(session::CREATED_AT, Direction::Desc)
        .order_by(session::SESSION_ID, Direction::Desc)
        .order_by(session::NODE_DID, Direction::Desc)
        .order_by(session::REQUESTER_DID, Direction::Desc)
        .order_by(session::DOC_ID, Direction::Desc)
}

#[derive(Debug, Clone)]
pub struct ScopedSessions {
    pub scopes: Vec<gents_protocol::session::SessionScope>,
    pub include_archived: bool,
}

impl View for ScopedSessions {
    type Collection = session::Doc;
    type Row = SessionRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query =
            Query::select(selection()).filter(Filter::any(self.scopes.iter().map(|scope| {
                gents_protocol::document_query::session_scope(
                    &scope.node_did,
                    &scope.session_id,
                    scope.requester_did.as_deref(),
                )
            })));
        if !self.include_archived {
            query = query.filter(Filter::is_null(session::ARCHIVED_AT));
        }
        ordered(query)
    }
}
