use gents_protocol::document_query::{fields::agent_tool_call as fields, Filter, Query, Selection};
use gents_protocol::views::View;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::background::job::view::JobKey;

#[derive(Debug, Deserialize)]
pub struct JobFields {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub tool_call_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub lifecycle_state: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub job_pin: Option<Value>,
    pub job_progress: Option<Value>,
}

pub struct JobRow {
    pub doc_id: String,
    pub fields: Result<JobFields, serde_json::Error>,
}

impl<'de> Deserialize<'de> for JobRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let doc_id = value
            .get("_docID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(Self {
            doc_id,
            fields: serde_json::from_value(value),
        })
    }
}

pub struct Jobs {
    pub key: Option<JobKey>,
    pub node_did: Option<String>,
    pub session_id: Option<String>,
}

impl View for Jobs {
    type Collection = fields::Doc;
    type Row = JobRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::doc_id()
                .field(fields::SESSION_ID)
                .field(fields::TOOL_CALL_ID)
                .field(fields::REQUEST_ID)
                .field(fields::REQUEST_DOC_ID)
                .field(fields::NODE_DID)
                .field(fields::REQUESTER_DID)
                .field(fields::LIFECYCLE_STATE)
                .field(fields::STARTED_AT)
                .field(fields::COMPLETED_AT)
                .field(fields::JOB_PIN)
                .field(fields::JOB_PROGRESS),
        );
        query = match &self.key {
            Some(JobKey::RowDocId(id)) => query.filter(Filter::eq(fields::DOC_ID, id)),
            Some(JobKey::Address {
                session_id,
                tool_call_id,
            }) => query
                .filter(Filter::eq(fields::SESSION_ID, session_id))
                .filter(Filter::eq(fields::TOOL_CALL_ID, tool_call_id)),
            None => query.filter(Filter::not_null(fields::JOB_PIN)),
        };
        if let Some(node) = &self.node_did {
            query = query.filter(Filter::eq(fields::NODE_DID, node));
        }
        if let Some(session) = &self.session_id {
            query = query.filter(Filter::eq(fields::SESSION_ID, session));
        }
        query
    }
}
