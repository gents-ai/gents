use gents_protocol::document_query::{fields::agent, Direction, Filter, Query, Selection};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub agent: crate::Agent,
}

pub struct AgentList {
    node_did: String,
    agent_did: Option<String>,
    after: Option<String>,
    doc_ids: Option<Vec<String>>,
    name: Option<(String, String)>,
    oldest_first: bool,
    limit: Option<u64>,
}

impl AgentList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            agent_did: None,
            after: None,
            doc_ids: None,
            name: None,
            oldest_first: false,
            limit: None,
        }
    }

    pub fn agent_did(mut self, agent_did: impl Into<String>) -> Self {
        self.agent_did = Some(agent_did.into());
        self
    }

    pub fn after(mut self, agent_did: impl Into<String>) -> Self {
        self.after = Some(agent_did.into());
        self
    }

    pub fn named(
        mut self,
        display_name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.name = Some((display_name.into(), description.into()));
        self
    }

    pub fn oldest_first(mut self) -> Self {
        self.oldest_first = true;
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

impl View for AgentList {
    type Collection = agent::Doc;
    type Row = AgentRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::doc_id()
                .field(agent::AGENT_DID)
                .field(agent::CREATION_NONCE)
                .field(agent::NODE_DID)
                .field(agent::DISPLAY_NAME)
                .field(agent::DESCRIPTION)
                .field(agent::CONTEXT_ID)
                .field(agent::INFERENCE_PROFILE_ID)
                .field(agent::ENABLED)
                .field(agent::TAGS)
                .field(agent::CREATED_AT)
                .field(agent::UPDATED_AT),
        )
        .filter(Filter::eq(agent::NODE_DID, &self.node_did));
        if self.oldest_first {
            query = query.order_by(agent::CREATED_AT, Direction::Asc);
        }
        query = query.order_by(agent::AGENT_DID, Direction::Asc);
        if let Some((name, description)) = &self.name {
            query = query
                .filter(Filter::eq(agent::DISPLAY_NAME, name))
                .filter(Filter::eq(agent::DESCRIPTION, description));
        }
        if let Some(agent_did) = &self.agent_did {
            query = query.filter(Filter::eq(agent::AGENT_DID, agent_did));
        }
        if let Some(after) = &self.after {
            query = query.filter(Filter::gt(agent::AGENT_DID, after));
        }
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(agent::DOC_ID, doc_ids.iter()));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
