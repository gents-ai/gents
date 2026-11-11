use std::sync::Arc;

use anyhow::Context;
use gents_protocol::goal::RequestAssignmentScope;
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigRead;
use gents_store::graphql::escape_graphql_string;
use rapidhash::{RapidHashMap, RapidHashSet};

/// The selection of a session's request rows that every request-ancestry
/// owner reads.
pub fn session_request_fields() -> String {
    format!(
        "{} failure_reason superseded_by_request superseded_by_request_doc_id",
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
    )
}

/// Every request row of one owner/session, newest first.
pub async fn load_session_request_rows<R>(
    txn: &R,
    node_did: &str,
    session_id: &str,
) -> anyhow::Result<Vec<AgentRequestRow>>
where
    R: ConfigRead,
{
    let fields = session_request_fields();
    let escaped_did = escape_graphql_string(node_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute_read(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{escaped_did}" }}, session_id: {{ _eq: "{escaped_session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {fields} }} }}"#
        ))
        .await?;
    serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("request query omitted rows")?,
    )
    .map_err(Into::into)
}

/// One owner/session's request rows as core read them, indexed by physical
/// document. A row of another owner or session, or without a document ID,
/// is kept but not indexed.
#[derive(Debug)]
pub struct SessionRequests {
    owner: String,
    session: String,
    rows: Vec<AgentRequestRow>,
    index: RapidHashMap<String, usize>,
}

impl SessionRequests {
    pub fn new(owner: &str, session: &str, rows: Vec<AgentRequestRow>) -> anyhow::Result<Self> {
        let mut index = RapidHashMap::default();
        for (position, row) in rows.iter().enumerate() {
            if row.node_did.as_deref() != Some(owner) || row.session_id.as_deref() != Some(session)
            {
                continue;
            }
            let Some(doc) = row.doc_id.as_deref().filter(|doc| !doc.is_empty()) else {
                continue;
            };
            anyhow::ensure!(
                index.insert(doc.to_owned(), position).is_none(),
                "duplicate physical request document in ancestry observation"
            );
        }
        Ok(Self {
            owner: owner.to_owned(),
            session: session.to_owned(),
            rows,
            index,
        })
    }

    pub async fn load<R>(txn: &R, owner: &str, session: &str) -> anyhow::Result<Self>
    where
        R: ConfigRead,
    {
        Self::new(
            owner,
            session,
            load_session_request_rows(txn, owner, session).await?,
        )
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn rows(&self) -> &[AgentRequestRow] {
        &self.rows
    }

    /// The indexed row of a physical request, without checks.
    pub fn row(&self, doc: &str) -> Option<&AgentRequestRow> {
        self.index.get(doc).map(|position| &self.rows[*position])
    }

    /// A normal request of this owner/session whose signed receipt verifies.
    pub fn request(&self, doc: &str) -> anyhow::Result<&AgentRequestRow> {
        let row = self
            .row(doc)
            .context("request is absent from its owner/session observation")?;
        anyhow::ensure!(
            row.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal),
            "assignment request is auxiliary"
        );
        gents_store::identity::request_receipt::verify_request_receipt_signature(row)?;
        Ok(row)
    }
}

/// A plugin's failure to determine one request's key, as the chain of
/// messages of the error it met, outermost first. Core rebuilds the error
/// with the same messages when it consults that request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFailure {
    pub chain: Vec<String>,
}

impl ScopeFailure {
    pub fn of(error: &anyhow::Error) -> Self {
        Self {
            chain: error.chain().map(ToString::to_string).collect(),
        }
    }

    fn error(&self) -> anyhow::Error {
        let mut messages = self.chain.iter().rev();
        let mut error = anyhow::Error::msg(messages.next().cloned().unwrap_or_default());
        for outer in messages {
            error = error.context(outer.clone());
        }
        error
    }
}

/// Lean `Hooks.QueueScope.Answer` for one request; a request absent from a
/// plugin's answers is unanswered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeAnswer {
    Keyed(RequestAssignmentScope),
    Failed(ScopeFailure),
}

/// One plugin's answers at `request.queue.scope`, by physical request.
pub type ScopeAnswers = RapidHashMap<String, ScopeAnswer>;

/// The queue partition of one request: the keys plugins answered, named by
/// plugin, in the fixed plugin order. Core compares partitions and does not
/// interpret a key, except `referenced` below. Empty is the unscoped default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Partition(Vec<(&'static str, RequestAssignmentScope)>);

impl Partition {
    pub fn is_unscoped(&self) -> bool {
        self.0.is_empty()
    }

    /// The first answered key that names an assignment reference. A wake
    /// enqueued for a request carries it in its signed input, so the wake's
    /// creation stamp names the partition its cause was answered with.
    pub fn referenced(&self) -> Option<&GoalAssignmentInput> {
        self.0.iter().find_map(|(_, key)| match key {
            RequestAssignmentScope::Referenced(reference) => Some(reference),
            RequestAssignmentScope::Unassigned | RequestAssignmentScope::Unresolved { .. } => None,
        })
    }
}

/// Lean `Hooks.QueueScope.partition`: the first failure in plugin order is
/// the request's error; otherwise the answered keys in plugin order.
pub fn partition(named: &[(&'static str, ScopeAnswers)], doc: &str) -> anyhow::Result<Partition> {
    let mut keys = Vec::new();
    for (plugin, answers) in named {
        match answers.get(doc) {
            None => {}
            Some(ScopeAnswer::Keyed(key)) => keys.push((*plugin, key.clone())),
            Some(ScopeAnswer::Failed(failure)) => return Err(failure.error()),
        }
    }
    Ok(Partition(keys))
}

/// What `request.queue.scope` returns to a queue or claim site: core's request
/// rows for the session and each plugin's answers for the requests asked.
#[derive(Debug)]
pub struct QueueScopes {
    requests: Arc<SessionRequests>,
    asked: RapidHashSet<String>,
    answers: Vec<(&'static str, ScopeAnswers)>,
}

impl QueueScopes {
    pub(crate) fn new(
        requests: Arc<SessionRequests>,
        asked: &[String],
        answers: Vec<(&'static str, ScopeAnswers)>,
    ) -> Self {
        Self {
            requests,
            asked: asked.iter().cloned().collect(),
            answers,
        }
    }

    pub fn request(&self, doc: &str) -> anyhow::Result<&AgentRequestRow> {
        self.requests.request(doc)
    }

    pub fn scope(&self, doc: &str) -> anyhow::Result<Partition> {
        self.requests.request(doc)?;
        anyhow::ensure!(
            self.asked.contains(doc),
            "request {doc} was not asked for its queue partition"
        );
        partition(&self.answers, doc)
    }

    pub fn scope_for_request(&self, doc: &str, request_id: &str) -> anyhow::Result<Partition> {
        anyhow::ensure!(
            self.requests.request(doc)?.request_id == request_id,
            "assignment request logical identity differs from its physical observation"
        );
        self.scope(doc)
    }
}
