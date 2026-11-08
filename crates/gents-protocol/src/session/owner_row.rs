use serde_json::Value;

use super::AgentSession;
use crate::graphql::escape_graphql_string;
use crate::row::AgentRequest;

/// Fields for one canonical `AgentSession` read. Nested values are DefraDB
/// JSON scalar columns: read the bare field name, never an object subselection.
pub const AGENT_SESSION_FIELDS: &str = "session_id node_did requester_did agent_did \
created_at closed_at stopped_at archived_at archive_revision title provenance observation tags _docID";

pub const SESSION_SCOPE_FIELDS: &str = "session_id node_did requester_did agent_did \
created_at closed_at title provenance observation tags _docID";

#[derive(Debug, thiserror::Error)]
pub enum SessionRowError {
    #[error("session {0} must be nonblank")]
    Blank(&'static str),
    #[error("session requester_did must be nonblank when present")]
    BlankRequester,
    #[error("session {field} is invalid")]
    InvalidTimestamp {
        field: &'static str,
        #[source]
        source: chrono::ParseError,
    },
    #[error("session archive revision exceeds the database integer range")]
    ArchiveRevisionOutOfRange,
    #[error("session preview exceeds the 240-character bound")]
    PreviewTooLong,
    #[error("session title text must be nonblank")]
    BlankTitle,
    #[error("AgentSession row omitted _docID")]
    MissingDocId,
    #[error("AgentSession row is not an object")]
    NotAnObject,
    #[error("decoding canonical AgentSession")]
    Decode(#[source] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct SessionOwnerRow {
    pub doc_id: String,
    pub session: AgentSession,
}

fn timestamp(field: &'static str, value: &str) -> Result<(), SessionRowError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(drop)
        .map_err(|source| SessionRowError::InvalidTimestamp { field, source })
}

pub fn validate_agent_session(session: &AgentSession) -> Result<(), SessionRowError> {
    for (name, value) in [
        ("session_id", session.session_id.as_str()),
        ("node_did", session.node_did.as_str()),
        ("agent_did", session.agent_did.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(SessionRowError::Blank(name));
        }
    }
    if session
        .requester_did
        .as_deref()
        .is_some_and(|requester| requester.trim().is_empty())
    {
        return Err(SessionRowError::BlankRequester);
    }
    timestamp("created_at", &session.created_at)?;
    if let Some(closed_at) = session.closed_at.as_deref() {
        timestamp("closed_at", closed_at)?;
    }
    if let Some(archived_at) = session.archived_at.as_deref() {
        timestamp("archived_at", archived_at)?;
    }
    if session.archive_revision > i64::MAX as u64 {
        return Err(SessionRowError::ArchiveRevisionOutOfRange);
    }
    if let Some(stopped_at) = session.stopped_at.as_deref() {
        timestamp("stopped_at", stopped_at)?;
    }
    if let Some(observation) = session.observation.as_ref() {
        timestamp("last_activity_at", &observation.last_activity_at)?;
        if observation
            .preview
            .as_deref()
            .is_some_and(|preview| preview.chars().count() > 240)
        {
            return Err(SessionRowError::PreviewTooLong);
        }
    }
    if session
        .title
        .as_ref()
        .is_some_and(|title| title.text.trim().is_empty())
    {
        return Err(SessionRowError::BlankTitle);
    }
    Ok(())
}

pub fn decode_session_row(row: &Value) -> Result<SessionOwnerRow, SessionRowError> {
    let doc_id = row
        .get("_docID")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(SessionRowError::MissingDocId)?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .ok_or(SessionRowError::NotAnObject)?
        .remove("_docID");
    let session: AgentSession = serde_json::from_value(document).map_err(SessionRowError::Decode)?;
    validate_agent_session(&session)?;
    Ok(SessionOwnerRow { doc_id, session })
}

pub fn session_scope_filter(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> String {
    let requester = match requester_did {
        Some(did) => format!("\"{}\"", escape_graphql_string(did)),
        None => "null".to_string(),
    };
    format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id)
    )
}

impl From<&super::AgentSession> for SessionScope {
    fn from(session: &super::AgentSession) -> Self {
        Self {
            node_did: session.node_did.clone(),
            session_id: session.session_id.clone(),
            requester_did: session.requester_did.clone(),
        }
    }
}

impl From<&super::SessionReference> for SessionScope {
    fn from(source: &super::SessionReference) -> Self {
        Self {
            node_did: source.node_did.clone(),
            session_id: source.session_id.clone(),
            requester_did: source.requester_did.clone(),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionScope {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl SessionScope {
    pub fn of_request(request: &AgentRequest) -> Self {
        Self {
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            requester_did: request.requester_did.clone(),
        }
    }

    pub fn of_row(row: &Value) -> Option<Self> {
        Some(Self {
            node_did: row.get("node_did")?.as_str()?.to_string(),
            session_id: row.get("session_id")?.as_str()?.to_string(),
            requester_did: row
                .get("requester_did")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}
