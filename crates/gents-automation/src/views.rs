use gents_protocol::document_query::{fields, Direction, Filter, Query, Selection};
use gents_protocol::views::View;

#[derive(Debug, Clone)]
pub struct GoalRows {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    session_ids: Option<Vec<String>>,
}

impl GoalRows {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            session_ids: None,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn sessions(mut self, session_ids: Vec<String>) -> Self {
        self.session_ids = Some(session_ids);
        self
    }
}

impl View for GoalRows {
    type Collection = fields::goal::Doc;
    type Row = gents_protocol::row::GoalRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use fields::goal as f;
        let mut query = Query::select(
            Selection::new()
                .field(f::GOAL_ID)
                .field(f::SESSION_ID)
                .field(f::NODE_DID)
                .field(f::CREATION_KEY)
                .field(f::OBJECTIVE)
                .field(f::STATUS)
                .field(f::TOKEN_BUDGET)
                .field(f::TOKENS_USED)
                .field(f::ACTIVE_TIME_SECONDS)
                .field(f::ACTIVE_STARTED_AT)
                .field(f::CONSECUTIVE_BLOCKED_AUDITS)
                .field(f::LAST_BLOCKED_REQUEST_ID)
                .field(f::LAST_BLOCKED_REASON)
                .field(f::LAST_CONTINUED_FROM_REQUEST_ID)
                .field(f::CONTINUATION_SEQUENCE)
                .field(f::WRAPUP_REQUESTED)
                .field(f::WRAPUP_COMPLETED)
                .field(f::INFRASTRUCTURE_RETRY_COUNT)
                .field(f::LAST_FAILURE)
                .field(f::COMPLETION_EVIDENCE)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::GOAL_ID, Direction::Asc);
        if let Some(ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, ids));
        }
        if let Some(ids) = &self.session_ids {
            query = query.filter(Filter::any_of(f::SESSION_ID, ids));
        }
        query
    }
}

#[derive(Debug, Clone)]
pub struct MailboxRows {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    session_ids: Option<Vec<String>>,
    open_only: bool,
}

impl MailboxRows {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            session_ids: None,
            open_only: false,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn sessions(mut self, session_ids: Vec<String>) -> Self {
        self.session_ids = Some(session_ids);
        self
    }

    pub fn open(mut self) -> Self {
        self.open_only = true;
        self
    }
}

impl View for MailboxRows {
    type Collection = fields::mailbox_item::Doc;
    type Row = gents_protocol::row::MailboxItemRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use fields::mailbox_item as f;
        let mut query = Query::select(
            Selection::new()
                .field(f::DOC_ID)
                .field(f::ITEM_KEY)
                .field(f::REQUESTER_DID)
                .field(f::NODE_DID)
                .field(f::STATUS)
                .field(f::KIND)
                .field(f::ACTION)
                .field(f::TITLE)
                .field(f::SUMMARY)
                .field(f::PAYLOAD)
                .field(f::SOURCE_KIND)
                .field(f::SOURCE_ID)
                .field(f::SESSION_ID)
                .field(f::REQUEST_ID)
                .field(f::CAUSE_DOC_ID)
                .field(f::TARGET_NODE_DID)
                .field(f::TARGET_AGENT_DID)
                .field(f::EXPECTED_COLLECTION)
                .field(f::PARENT_ITEM_ID)
                .field(f::DEADLINE_AT)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT)
                .field(f::RESOLVED_AT)
                .field(f::RESOLVED_DOC_ID),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::ITEM_KEY, Direction::Asc);
        if let Some(ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, ids));
        }
        if let Some(ids) = &self.session_ids {
            query = query.filter(Filter::any_of(f::SESSION_ID, ids));
        }
        if self.open_only {
            query = query.filter(Filter::eq(f::STATUS, "open"));
        }
        query
    }
}
