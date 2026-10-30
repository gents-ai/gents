use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigRead};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::session::{
    decode_session_row, public_request_filter, session_scope_filter, AGENT_SESSION_FIELDS,
};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::AgentSession;

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionScope {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl SessionScope {
    pub fn of_request(request: &crate::AgentRequest) -> Self {
        Self {
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            requester_did: request.requester_did.clone(),
        }
    }

    fn of_row(row: &Value) -> Option<Self> {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLink {
    pub scope: SessionScope,
    pub cause_request_doc_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequestCause {
    pub request_id: String,
    pub cause_request_doc_id: String,
    pub cause_scope: Option<SessionScope>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLineage {
    pub started_by: Option<SessionLink>,
    pub started: Vec<SessionLink>,
    pub sent: Vec<SessionLink>,
    pub received: Vec<SessionLink>,
    pub request_causes: Vec<SessionRequestCause>,
}

const SCOPE_FIELDS: &str = "_docID node_did session_id requester_did";

async fn collection<R>(access: &R, query: &str, name: &str) -> Result<Vec<Value>>
where
    R: ConfigRead + ?Sized,
{
    Ok(access
        .execute_read(query)
        .await?
        .pointer(&format!("/data/{name}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn session_rows(
    access: &ConfigAccess,
    filter: &str,
) -> Result<Vec<crate::session::SessionOwnerRow>> {
    collection(
        access,
        &format!("{{AgentSession(filter: {{{filter}}}) {{{AGENT_SESSION_FIELDS}}}}}"),
        "AgentSession",
    )
    .await?
    .iter()
    .map(decode_session_row)
    .collect()
}

async fn sessions(access: &ConfigAccess, filter: &str) -> Result<Vec<AgentSession>> {
    Ok(session_rows(access, filter)
        .await?
        .into_iter()
        .map(|row| row.session)
        .collect())
}

/// The immutable starter index columns [`recorded_starter`] reads from the
/// same row as [`AGENT_SESSION_FIELDS`].
pub const STARTER_INDEX_FIELDS: &str = "started_by_node_did started_by_session_id";

impl From<&AgentSession> for SessionScope {
    fn from(session: &AgentSession) -> Self {
        Self {
            node_did: session.node_did.clone(),
            session_id: session.session_id.clone(),
            requester_did: session.requester_did.clone(),
        }
    }
}

impl From<&gents_protocol::session::SessionReference> for SessionScope {
    fn from(source: &gents_protocol::session::SessionReference) -> Self {
        Self {
            node_did: source.node_did.clone(),
            session_id: source.session_id.clone(),
            requester_did: source.requester_did.clone(),
        }
    }
}

fn decode_indexed_session(row: &Value) -> Result<crate::session::SessionOwnerRow> {
    let mut row = row.clone();
    if let Some(fields) = row.as_object_mut() {
        fields.remove("started_by_node_did");
        fields.remove("started_by_session_id");
    }
    decode_session_row(&row)
}

/// The verified starter of one `AgentSession` row selected with
/// [`AGENT_SESSION_FIELDS`] and [`STARTER_INDEX_FIELDS`] (Lean `AgentSession.observedStarter`): the
/// immutable index columns, never the mutable provenance, select verification,
/// and an unindexed row has no starter.
pub fn recorded_starter(row: &Value) -> Option<SessionLink> {
    let index = |field: &str| row.get(field).and_then(Value::as_str);
    let (Some(_), Some(_)) = (index("started_by_node_did"), index("started_by_session_id")) else {
        return None;
    };
    let owner = decode_indexed_session(row).ok()?;
    let provenance = owner.session.provenance.as_ref()?;
    let receipt = provenance.creation_receipt.as_ref()?;
    let verified = verified_creation_receipt(&owner.session, &owner.doc_id, receipt.cause)?;
    let starter = verified.starter.as_ref()?;
    if provenance.started_by.as_ref() != Some(&starter.source)
        || provenance.parent_request_doc_id.as_deref()
            != Some(starter.parent_request_doc_id.as_str())
        || provenance.opening_request_doc_id.as_deref()
            != Some(verified.opening_request_doc_id.as_str())
        || row.get("started_by_node_did").and_then(Value::as_str)
            != Some(starter.source.node_did.as_str())
        || row.get("started_by_session_id").and_then(Value::as_str)
            != Some(starter.source.session_id.as_str())
    {
        return None;
    }
    Some(SessionLink {
        scope: SessionScope::from(&starter.source),
        cause_request_doc_id: starter.parent_request_doc_id.clone(),
    })
}

pub async fn verified_recorded_starter<R>(
    access: &R,
    scope: &SessionScope,
) -> Result<Option<SessionLink>>
where
    R: ConfigRead + ?Sized,
{
    let filter = session_scope_filter(
        &scope.node_did,
        &scope.session_id,
        scope.requester_did.as_deref(),
    );
    let rows = collection(
        access,
        &format!("{{AgentSession(filter: {{{filter}}}, limit: 2) {{{AGENT_SESSION_FIELDS} {STARTER_INDEX_FIELDS}}}}}"),
        "AgentSession",
    )
    .await?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous session starter scope");
    Ok(rows.first().and_then(|row| {
        let owner = decode_indexed_session(row).ok()?;
        (SessionScope::from(&owner.session) == *scope)
            .then(|| recorded_starter(row))
            .flatten()
    }))
}

/// One read session row and its verified starter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedStarter {
    pub session: AgentSession,
    pub starter: Option<SessionLink>,
}

/// [`verified_recorded_starter`] for many scopes in batched reads. A scope
/// with no row, or with more than one, is omitted.
pub async fn verified_recorded_starters<R>(
    access: &R,
    scopes: &[SessionScope],
) -> Result<Vec<ObservedStarter>>
where
    R: ConfigRead + ?Sized,
{
    let wanted = scopes.iter().collect::<BTreeSet<_>>();
    let mut found = BTreeMap::<SessionScope, Vec<ObservedStarter>>::new();
    for batch in wanted.iter().copied().collect::<Vec<_>>().chunks(64) {
        let filters = batch
            .iter()
            .map(|scope| {
                format!(
                    "{{{}}}",
                    session_scope_filter(
                        &scope.node_did,
                        &scope.session_id,
                        scope.requester_did.as_deref(),
                    )
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "{{AgentSession(filter: {{_or: [{filters}]}}) {{{AGENT_SESSION_FIELDS} {STARTER_INDEX_FIELDS}}}}}"
        );
        for row in collection(access, &query, "AgentSession").await? {
            let Ok(owner) = decode_indexed_session(&row) else {
                continue;
            };
            let scope = SessionScope::from(&owner.session);
            if wanted.contains(&scope) {
                found.entry(scope).or_default().push(ObservedStarter {
                    starter: recorded_starter(&row),
                    session: owner.session,
                });
            }
        }
    }
    Ok(found
        .into_values()
        .filter_map(|rows| <[ObservedStarter; 1]>::try_from(rows).ok())
        .map(|[only]| only)
        .collect())
}

pub async fn request_scopes<'a, R>(
    access: &R,
    doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<(String, SessionScope)>>
where
    R: ConfigRead + ?Sized,
{
    let doc_ids = doc_ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut scopes = Vec::new();
    for batch in doc_ids.chunks(128) {
        let query = format!(
            "{{AgentRequest(docIDs: {}) {{{SCOPE_FIELDS}}}}}",
            graphql_string_list_literal(batch.iter().copied())
        );
        scopes.extend(
            collection(access, &query, "AgentRequest")
                .await?
                .iter()
                .filter_map(|row| {
                    Some((
                        row.get("_docID")?.as_str()?.to_owned(),
                        SessionScope::of_row(row)?,
                    ))
                }),
        );
    }
    Ok(scopes)
}

pub async fn load_session(
    access: &ConfigAccess,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<AgentSession>> {
    let mut filter = format!(
        r#"session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(session_id)
    );
    if let Some(requester) = requester_did {
        filter.push_str(&format!(
            r#", requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(requester)
        ));
    }
    let mut rows = sessions(access, &filter).await?;
    anyhow::ensure!(rows.len() <= 1, "session {session_id} is ambiguous");
    Ok(rows.pop())
}

pub async fn started_by(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<SessionLink>> {
    verified_recorded_starter(access, scope).await
}

/// How a session was created, from its verified creation receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreationOrigin {
    pub cause: gents_protocol::session::SessionCreationCause,
    /// The session whose agent opened this one, when the receipt names it.
    pub starter: Option<SessionLink>,
}

/// The verified creation of `scope`'s session; `None` when it carries no verified receipt,
/// which is how a session a client opened looks.
pub async fn verified_creation_origin(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<CreationOrigin>> {
    let rows = session_rows(
        access,
        &session_scope_filter(
            &scope.node_did,
            &scope.session_id,
            scope.requester_did.as_deref(),
        ),
    )
    .await?;
    let [owner] = rows.as_slice() else {
        return Ok(None);
    };
    let Some(cause) = owner
        .session
        .provenance
        .as_ref()
        .and_then(|provenance| provenance.creation_receipt.as_ref())
        .map(|receipt| receipt.cause)
    else {
        return Ok(None);
    };
    if verified_creation_receipt(&owner.session, &owner.doc_id, cause).is_none() {
        return Ok(None);
    }
    Ok(Some(CreationOrigin {
        cause,
        starter: verified_recorded_starter(access, scope).await?,
    }))
}

/// The opening request `scope`'s session recorded in its provenance when it
/// was created (Lean `AgentSession.Provenance.openingRequestDoc`). It reads the
/// session by its exact scope, as [`started_by`] does, so a session with the
/// same label on another node is never read.
pub async fn recorded_opening(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<String>> {
    let own = sessions(
        access,
        &session_scope_filter(
            &scope.node_did,
            &scope.session_id,
            scope.requester_did.as_deref(),
        ),
    )
    .await?;
    Ok(own
        .first()
        .and_then(|session| session.provenance.as_ref()?.opening_request_doc_id.clone()))
}

pub async fn task_triggered_opening(access: &ConfigAccess, scope: &SessionScope) -> Result<bool> {
    let own = session_rows(
        access,
        &session_scope_filter(
            &scope.node_did,
            &scope.session_id,
            scope.requester_did.as_deref(),
        ),
    )
    .await?;
    let [owner] = own.as_slice() else {
        return Ok(false);
    };
    let session = &owner.session;
    let Some(receipt) = verified_creation_receipt(
        session,
        &owner.doc_id,
        gents_protocol::session::SessionCreationCause::TaskTrigger,
    ) else {
        return Ok(false);
    };
    let opening = receipt.opening_request_doc_id.as_str();
    let rows = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(opening),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Ok(false);
    };
    let request: AgentRequestRow = match serde_json::from_value(row.clone()) {
        Ok(request) => request,
        Err(_) => return Ok(false),
    };
    let Some(trigger_id) = request
        .caused_by_trigger_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(false);
    };
    if request.node_did.as_deref() != Some(scope.node_did.as_str())
        || request.session_id.as_deref() != Some(scope.session_id.as_str())
        || request.requester_did != scope.requester_did
        || request.agent_id.as_deref() != Some(session.agent_id.as_str())
        || request.admission_kind.as_deref() != Some("runtime-internal")
        || request.admission_signer_did.as_deref() != Some(scope.node_did.as_str())
        || request.runtime_issuer_did.as_deref() != Some(scope.node_did.as_str())
        || request.runtime_source_kind.as_deref() != Some("automated-trigger")
        || request.runtime_source_request_id.as_deref() != Some(trigger_id)
        || !matches!(
            request.caused_by_trigger_kind.as_deref(),
            Some("event" | "schedule")
        )
        || crate::request_admission::verify_request_receipt_signature(&request).is_err()
    {
        return Ok(false);
    }
    Ok(task_creation_binding(access, &request).await?.as_deref()
        == Some(receipt.cause_binding.as_str()))
}

/// The `AgentRequestRow` fields lineage readers select.
pub const REQUEST_FIELDS: &str = "_docID request_id node_did session_id requester_did agent_id \
     content lifecycle_state superseded_by_request failure_reason created_at request_hop \
     caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

/// The public request that started a linked session: among the requests of
/// `link.scope` that name the link's cause and a tool call, the one the
/// session's provenance recorded when it was created, else the earliest by
/// `(created_at, _docID)` for a session created before that was recorded. It
/// is never found by tool name, so a call of any kind may open it, and a later
/// message from the same cause into the same session, even in the same second,
/// does not displace a recorded opening request.
pub async fn starting_request(
    access: &ConfigAccess,
    link: &SessionLink,
) -> Result<Option<AgentRequestRow>> {
    let filter = public_request_filter(&format!(
        r#"{}, caused_by_parent_request_doc_id: {{ _eq: "{}" }}"#,
        session_scope_filter(
            &link.scope.node_did,
            &link.scope.session_id,
            link.scope.requester_did.as_deref(),
        ),
        escape_graphql_string(&link.cause_request_doc_id)
    ));
    let mut rows = access
        .execute(&format!(
            "{{AgentRequest(filter: {{{filter}}}) {{{REQUEST_FIELDS}}}}}"
        ))
        .await?
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let opening = recorded_opening(access, &link.scope).await?;
    for row in &mut rows {
        row["opening_request_doc_id"] = opening.clone().map_or(Value::Null, Value::String);
    }
    opening_request(&rows, link)
        .map(|row| serde_json::from_value(row).context("decoding AgentRequest row"))
        .transpose()
}

/// The opening request among request rows (`_docID`, `created_at`, the
/// session scope, both causal edges and the session's recorded
/// `opening_request_doc_id`), as Lean `DurableLineage.openingRequest` states
/// it: among rows of `link.scope` whose `caused_by_parent_request_doc_id` is
/// the link's cause and whose `caused_by_parent_tool_call_doc_id` is non-null,
/// the recorded opening request, and none while it is not among them; without
/// a recorded one, the earliest by `(created_at, _docID)`.
pub fn opening_request(rows: &[Value], link: &SessionLink) -> Option<Value> {
    let text = |row: &Value, field: &str| row.get(field).and_then(Value::as_str).map(str::to_owned);
    let candidates: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            text(row, "caused_by_parent_request_doc_id").as_deref()
                == Some(&link.cause_request_doc_id)
                && text(row, "caused_by_parent_tool_call_doc_id").is_some()
                && text(row, "node_did").as_deref() == Some(&link.scope.node_did)
                && text(row, "session_id").as_deref() == Some(&link.scope.session_id)
                && text(row, "requester_did") == link.scope.requester_did
        })
        .collect();
    match candidates
        .iter()
        .find_map(|row| text(row, "opening_request_doc_id"))
    {
        Some(opening) => candidates
            .into_iter()
            .find(|row| text(row, "_docID").as_deref() == Some(opening.as_str()))
            .cloned(),
        None => candidates
            .into_iter()
            .min_by_key(|row| (text(row, "created_at"), text(row, "_docID")))
            .cloned(),
    }
}

/// One public request a physical tool call caused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausedRequest {
    pub request_id: String,
    pub request_doc_id: String,
    pub scope: SessionScope,
    pub created_at: String,
}

/// Group request rows by `caused_by_parent_tool_call_doc_id`, each list
/// ordered by `(created_at, request_doc_id)`.
pub fn group_caused(rows: &[Value]) -> BTreeMap<String, Vec<CausedRequest>> {
    let mut grouped = BTreeMap::<String, Vec<CausedRequest>>::new();
    for row in rows {
        let text = |field: &str| row.get(field).and_then(Value::as_str).map(str::to_owned);
        let (Some(call), Some(request_id), Some(request_doc_id), Some(created_at), Some(scope)) = (
            text("caused_by_parent_tool_call_doc_id"),
            text("request_id"),
            text("_docID"),
            text("created_at"),
            SessionScope::of_row(row),
        ) else {
            continue;
        };
        grouped.entry(call).or_default().push(CausedRequest {
            request_id,
            request_doc_id,
            scope,
            created_at,
        });
    }
    for requests in grouped.values_mut() {
        requests.sort_by(|a, b| {
            (&a.created_at, &a.request_doc_id).cmp(&(&b.created_at, &b.request_doc_id))
        });
    }
    grouped
}

/// Every public request each physical tool call caused: the requests whose
/// `caused_by_parent_tool_call_doc_id` names that call.
pub async fn requests_caused_by_calls<'a>(
    access: &ConfigAccess,
    call_doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, Vec<CausedRequest>>> {
    let call_doc_ids = call_doc_ids.into_iter().collect::<BTreeSet<_>>();
    if call_doc_ids.is_empty() {
        return Ok(Default::default());
    }
    let query = format!(
        "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} request_id created_at caused_by_parent_tool_call_doc_id}}}}",
        public_request_filter(&format!(
            "caused_by_parent_tool_call_doc_id: {{ _in: {} }}",
            graphql_string_list_literal(call_doc_ids)
        ))
    );
    Ok(group_caused(
        &collection(access, &query, "AgentRequest").await?,
    ))
}

/// The single-request projection, for session-message calls (`agent_new`,
/// `agent_message`); one call may cause many requests
/// ([`requests_caused_by_calls`]). A call named by more than one request has
/// no single caused request.
pub fn single_caused(requests: Vec<CausedRequest>) -> Option<CausedRequest> {
    <[CausedRequest; 1]>::try_from(requests)
        .ok()
        .map(|[only]| only)
}

/// [`single_caused`] over every call's requests; a call without one is omitted.
pub async fn caused_requests<'a>(
    access: &ConfigAccess,
    tool_call_doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>> {
    Ok(requests_caused_by_calls(access, tool_call_doc_ids)
        .await?
        .into_iter()
        .filter_map(|(call, requests)| Some((call, single_caused(requests)?.request_id)))
        .collect())
}

pub async fn lineage(access: &ConfigAccess, scope: &SessionScope) -> Result<SessionLineage> {
    let own_filter = session_scope_filter(
        &scope.node_did,
        &scope.session_id,
        scope.requester_did.as_deref(),
    );
    let started_by = started_by(access, scope).await?;

    let own_requests = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} request_id caused_by_parent_request_doc_id}}}}",
            public_request_filter(&own_filter)
        ),
        "AgentRequest",
    )
    .await?;
    let own_doc_ids = own_requests
        .iter()
        .filter_map(|row| row.get("_docID")?.as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let causes = own_requests
        .iter()
        .filter_map(|row| row.get("caused_by_parent_request_doc_id")?.as_str())
        .filter(|doc_id| !own_doc_ids.contains(*doc_id))
        .collect::<BTreeSet<_>>();
    let mut cause_scopes = request_scopes(
        access,
        causes.into_iter().filter(|cause| {
            started_by
                .as_ref()
                .is_none_or(|source| source.cause_request_doc_id != *cause)
        }),
    )
    .await?;
    if let Some(source) = &started_by {
        cause_scopes.push((source.cause_request_doc_id.clone(), source.scope.clone()));
    }
    let cause_scopes_by_doc = cause_scopes
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    let request_causes = own_requests
        .iter()
        .filter_map(|row| {
            Some(SessionRequestCause {
                request_id: row.get("request_id")?.as_str()?.to_owned(),
                cause_request_doc_id: row
                    .get("caused_by_parent_request_doc_id")?
                    .as_str()?
                    .to_owned(),
                cause_scope: cause_scopes_by_doc
                    .get(row.get("caused_by_parent_request_doc_id")?.as_str()?)
                    .cloned(),
            })
        })
        .collect::<Vec<_>>();
    let mut received = Vec::new();
    for (cause, cause_scope) in cause_scopes {
        if cause_scope == *scope
            || started_by
                .as_ref()
                .is_some_and(|link| link.scope == cause_scope)
            || received
                .iter()
                .any(|link: &SessionLink| link.scope == cause_scope)
        {
            continue;
        }
        received.push(SessionLink {
            scope: cause_scope,
            cause_request_doc_id: cause,
        });
    }
    let mut started = Vec::new();
    let indexed_filter = format!(
        r#"started_by_node_did: {{ _eq: "{}" }}, started_by_session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(&scope.node_did),
        escape_graphql_string(&scope.session_id),
    );
    let indexed = collection(
        access,
        &format!("{{AgentSession(filter: {{{indexed_filter}}}) {{{AGENT_SESSION_FIELDS} {STARTER_INDEX_FIELDS}}}}}"),
        "AgentSession",
    )
    .await?;
    for row in indexed {
        if let (Some(source), Ok(owner)) = (recorded_starter(&row), decode_indexed_session(&row)) {
            if source.scope == *scope {
                started.push(SessionLink {
                    scope: SessionScope::from(&owner.session),
                    cause_request_doc_id: source.cause_request_doc_id,
                });
            }
        }
    }
    if own_doc_ids.is_empty() {
        return Ok(SessionLineage {
            started_by,
            started,
            received,
            request_causes,
            ..Default::default()
        });
    }

    let mut sent = Vec::new();
    let query = format!(
        "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} caused_by_parent_request_doc_id}}}}",
        public_request_filter(&format!(
            "caused_by_parent_request_doc_id: {{ _in: {} }}",
            graphql_string_list_literal(own_doc_ids.iter().map(String::as_str))
        ))
    );
    for row in collection(access, &query, "AgentRequest").await? {
        let (Some(target), Some(cause)) = (
            SessionScope::of_row(&row),
            row.get("caused_by_parent_request_doc_id")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        if target == *scope
            || started.iter().any(|link| link.scope == target)
            || sent.iter().any(|link: &SessionLink| link.scope == target)
        {
            continue;
        }
        sent.push(SessionLink {
            scope: target,
            cause_request_doc_id: cause.to_owned(),
        });
    }
    Ok(SessionLineage {
        started_by,
        started,
        sent,
        received,
        request_causes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defra_node::EmbeddedNode;
    use gents_protocol::session::SessionProvenance;
    use serde_json::json;

    async fn create(node: &EmbeddedNode, mutation: String, collection: &str) -> String {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        crate::graphql::single_mutation_document(&response, &format!("create_{collection}"))
            .unwrap()
            .and_then(|document| document["_docID"].as_str())
            .unwrap()
            .to_owned()
    }

    async fn seed(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        created_at: &str,
        cause_doc_id: Option<&str>,
    ) -> String {
        let cause = cause_doc_id
            .map(|doc_id| {
                format!(
                    r#"caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "{}", caused_by_parent_tool_call_id: "call-1","#,
                    escape_graphql_string(doc_id)
                )
            })
            .unwrap_or_default();
        create(
            node,
            format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn seed_session(node: &EmbeddedNode, session_id: &str, parent: Option<&str>) {
        let session = AgentSession {
            session_id: session_id.into(),
            node_did: "did:test:node".into(),
            requester_did: Some("did:test:node".into()),
            agent_id: "worker".into(),
            created_at: "2026-09-26T00:00:00Z".into(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: parent.map(|doc_id| SessionProvenance {
                parent_request_doc_id: Some(doc_id.into()),
                ..Default::default()
            }),
            observation: None,
        };
        let input = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(&session).unwrap(),
        )
        .unwrap();
        create(
            node,
            format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
            "AgentSession",
        )
        .await;
    }

    async fn seed_opened_session(
        node: &EmbeddedNode,
        node_did: &str,
        session_id: &str,
        opening: &str,
    ) {
        let session = AgentSession {
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: Some("did:test:node".into()),
            agent_id: "worker".into(),
            created_at: "2026-09-26T00:00:00Z".into(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: Some(SessionProvenance {
                parent_request_doc_id: Some("bae-parent".into()),
                opening_request_doc_id: Some(opening.into()),
                ..Default::default()
            }),
            observation: None,
        };
        let input = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(&session).unwrap(),
        )
        .unwrap();
        create(
            node,
            format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
            "AgentSession",
        )
        .await;
    }

    fn caused_row(doc_id: &str, request_id: &str, created_at: &str, call: &str) -> Value {
        serde_json::json!({
            "_docID": doc_id,
            "request_id": request_id,
            "node_did": "did:test:node",
            "session_id": "child",
            "requester_did": "did:test:node",
            "created_at": created_at,
            "caused_by_parent_request_doc_id": "bae-parent",
            "caused_by_parent_tool_call_doc_id": call,
        })
    }

    #[test]
    fn group_caused_orders_by_created_at_then_doc_id() {
        let grouped = group_caused(&[
            caused_row("bae-a", "r1", "2026-09-26T00:00:03Z", "call-1"),
            caused_row("bae-c", "r3", "2026-09-26T00:00:01Z", "call-1"),
            caused_row("bae-b", "r2", "2026-09-26T00:00:01Z", "call-1"),
        ]);
        let doc_ids = grouped["call-1"]
            .iter()
            .map(|request| request.request_doc_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(doc_ids, ["bae-b", "bae-c", "bae-a"]);
    }

    #[test]
    fn group_caused_keeps_every_request_of_one_call() {
        let grouped = group_caused(&[
            caused_row("bae-a", "r1", "2026-09-26T00:00:01Z", "call-1"),
            caused_row("bae-b", "r2", "2026-09-26T00:00:02Z", "call-1"),
            caused_row("bae-c", "r3", "2026-09-26T00:00:03Z", "call-2"),
        ]);
        let ids = |call: &str| {
            grouped[call]
                .iter()
                .map(|request| request.request_id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("call-1"), ["r1", "r2"]);
        assert_eq!(ids("call-2"), ["r3"]);
        assert_eq!(grouped["call-1"][0].scope, scope("child"));
    }

    #[tokio::test]
    async fn caused_requests_projection_omits_a_call_with_two_requests() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        for (request_id, call) in [("a1", "call-one"), ("b1", "call-two"), ("b2", "call-two")] {
            create(
                &node,
                format!(
                    r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "child", caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "bae-parent", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call}", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
                ),
                "AgentRequest",
            )
            .await;
        }
        let access = ConfigAccess::local(node.clone());
        let projection = caused_requests(&access, ["call-one", "call-two"])
            .await
            .unwrap();
        assert_eq!(
            projection,
            std::collections::BTreeMap::from([("call-one".to_owned(), "a1".to_owned())])
        );
        node.shutdown().await;
    }

    fn scope(session_id: &str) -> SessionScope {
        SessionScope {
            node_did: "did:test:node".into(),
            session_id: session_id.into(),
            requester_did: Some("did:test:node".into()),
        }
    }

    fn ids(links: &[SessionLink]) -> Vec<&str> {
        links
            .iter()
            .map(|link| link.scope.session_id.as_str())
            .collect()
    }

    #[tokio::test]
    async fn physical_request_scope_batch_keeps_requested_missing_and_foreign_rows_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let requested = seed(
            &node,
            "requested",
            "requested-session",
            "2026-09-26T00:00:00Z",
            None,
        )
        .await;
        let _foreign = seed(
            &node,
            "foreign",
            "foreign-session",
            "2026-09-26T00:00:01Z",
            None,
        )
        .await;
        let access = ConfigAccess::local(node.clone());

        let missing =
            document::DocID::new_v0_from_seed("missing-session-origin-request").to_string();
        let scopes = request_scopes(&access, [requested.as_str(), missing.as_str()])
            .await
            .unwrap();
        assert_eq!(
            scopes,
            vec![(requested, scope("requested-session"))],
            "the direct-ID read must return only found requested documents with their stored scope"
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn a_parent_pointer_without_a_verified_starter_is_only_messaging() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let parent = seed(&node, "p1", "parent", "2026-09-26T00:00:01Z", None).await;
        seed_session(&node, "parent", None).await;
        seed(&node, "s1", "existing", "2026-09-26T00:00:00Z", None).await;
        seed(
            &node,
            "s2",
            "existing",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed_session(&node, "existing", None).await;
        seed(
            &node,
            "c1",
            "started",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed_session(&node, "started", Some(&parent)).await;
        let other = seed(&node, "o1", "other", "2026-09-26T00:00:01Z", None).await;
        seed_session(&node, "other", None).await;
        seed(&node, "a0", "started", "2026-09-26T00:00:02Z", Some(&other)).await;
        let access = ConfigAccess::local(node.clone());

        let from_parent = lineage(&access, &scope("parent")).await.unwrap();
        assert_eq!(from_parent.started_by, None);
        assert!(from_parent.started.is_empty());
        let mut sent = ids(&from_parent.sent);
        sent.sort_unstable();
        assert_eq!(sent, ["existing", "started"]);
        assert!(from_parent.received.is_empty());

        let from_started = lineage(&access, &scope("started")).await.unwrap();
        assert_eq!(from_started.started_by, None);
        let mut received = ids(&from_started.received);
        received.sort_unstable();
        assert_eq!(received, ["other", "parent"]);

        let from_existing = lineage(&access, &scope("existing")).await.unwrap();
        assert_eq!(from_existing.started_by, None);
        assert_eq!(ids(&from_existing.received), ["parent"]);
        node.shutdown().await;
    }

    /// The recorded opening request is read by the session's exact scope: a
    /// session with the same label on another node is never read, and the
    /// label-only read is ambiguous there.
    #[tokio::test]
    async fn recorded_opening_reads_the_exact_session_scope() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        seed_opened_session(&node, "did:test:node", "child", "bae-own-opening").await;
        seed_opened_session(&node, "did:test:peer", "child", "bae-peer-opening").await;
        seed_session(&node, "plain", None).await;
        let access = ConfigAccess::local(node.clone());

        assert_eq!(
            recorded_opening(&access, &scope("child")).await.unwrap(),
            Some("bae-own-opening".to_owned())
        );
        assert_eq!(
            recorded_opening(&access, &scope("plain")).await.unwrap(),
            None
        );
        assert_eq!(
            recorded_opening(&access, &scope("missing")).await.unwrap(),
            None
        );
        assert!(load_session(&access, "child", Some("did:test:node"))
            .await
            .is_err());
        node.shutdown().await;
    }

    fn link() -> SessionLink {
        SessionLink {
            scope: SessionScope {
                node_did: "did:test:node".into(),
                session_id: "child".into(),
                requester_did: Some("did:test:node".into()),
            },
            cause_request_doc_id: "bae-parent".into(),
        }
    }

    fn row(doc_id: &str, created_at: &str, call: Option<&str>) -> Value {
        json!({
            "_docID": doc_id,
            "node_did": "did:test:node",
            "session_id": "child",
            "requester_did": "did:test:node",
            "created_at": created_at,
            "caused_by_parent_request_doc_id": "bae-parent",
            "caused_by_parent_tool_call_doc_id": call,
        })
    }

    fn doc_id(row: Option<Value>) -> Option<String> {
        row.map(|row| row["_docID"].as_str().unwrap().to_owned())
    }

    #[test]
    fn opening_request_prefers_agent_new_over_a_later_message() {
        let rows = [
            row("bae-message", "2026-09-26T00:00:02Z", Some("call-message")),
            row("bae-new", "2026-09-26T00:00:01Z", Some("call-new")),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-new")
        );
    }

    #[test]
    fn opening_request_is_the_earliest_of_one_call() {
        let rows = [
            row("bae-b", "2026-09-26T00:00:01Z", Some("call-1")),
            row("bae-c", "2026-09-26T00:00:02Z", Some("call-1")),
            row("bae-a", "2026-09-26T00:00:01Z", Some("call-1")),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-a")
        );
    }

    #[test]
    fn opening_request_needs_the_cause_the_call_edge_and_the_session() {
        let mut other_cause = row("bae-cause", "2026-09-26T00:00:00Z", Some("call-1"));
        other_cause["caused_by_parent_request_doc_id"] = json!("bae-other");
        let mut other_session = row("bae-session", "2026-09-26T00:00:00Z", Some("call-1"));
        other_session["session_id"] = json!("elsewhere");
        let rows = [
            other_cause,
            other_session,
            row("bae-no-call", "2026-09-26T00:00:00Z", None),
        ];
        assert_eq!(opening_request(&rows, &link()), None);
    }

    fn recording_opening(mut row: Value) -> Value {
        row["opening_request_doc_id"] = json!("bae-b-new");
        row
    }

    #[test]
    fn opening_request_follows_the_recorded_opening_in_one_second() {
        let rows = [
            recording_opening(row(
                "bae-a-message",
                "2026-09-26T00:00:01Z",
                Some("call-message"),
            )),
            recording_opening(row("bae-b-new", "2026-09-26T00:00:01Z", Some("call-new"))),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-b-new")
        );
    }

    #[test]
    fn opening_request_follows_the_recorded_opening_over_an_earlier_clock() {
        let rows = [
            recording_opening(row(
                "bae-a-message",
                "2026-09-26T00:00:00Z",
                Some("call-message"),
            )),
            recording_opening(row("bae-b-new", "2026-09-26T00:00:01Z", Some("call-new"))),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-b-new")
        );
    }

    #[test]
    fn opening_request_is_none_while_the_recorded_opening_is_not_visible() {
        let rows = [recording_opening(row(
            "bae-a-message",
            "2026-09-26T00:00:00Z",
            Some("call-message"),
        ))];
        assert_eq!(opening_request(&rows, &link()), None);
    }
}

pub(crate) async fn verified_starter_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    provenance: &gents_protocol::session::SessionProvenance,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    agent_id: &str,
) -> Result<Option<gents_protocol::session::SessionReference>> {
    let (Some(source), Some(opening), Some(parent)) = (
        provenance.started_by.as_ref(),
        provenance.opening_request_doc_id.as_deref(),
        provenance.parent_request_doc_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let response = txn
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(opening),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let Some([row]) = response["data"]["AgentRequest"]
        .as_array()
        .map(Vec::as_slice)
    else {
        return Ok(None);
    };
    let Ok(request) = serde_json::from_value::<AgentRequestRow>(row.clone()) else {
        return Ok(None);
    };
    if request.node_did.as_deref() != Some(node_did)
        || request.session_id.as_deref() != Some(session_id)
        || request.requester_did.as_deref() != requester_did
        || request.agent_id.as_deref() != Some(agent_id)
        || request.requester_did.as_deref() != Some(source.node_did.as_str())
        || request.admission_signer_did.as_deref() != Some(source.node_did.as_str())
        || request.caused_by_parent_request_doc_id.as_deref() != Some(parent)
        || request.caused_by_parent_tool_call_doc_id.is_none()
        || request
            .input
            .as_ref()
            .and_then(|input| input.started_by.as_ref())
            != Some(source)
        || source.session_id.is_empty()
        || crate::request_admission::verify_request_receipt_signature(&request).is_err()
    {
        return Ok(None);
    }
    Ok(Some(source.clone()))
}

pub(crate) async fn issue_creation_receipt_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    session_doc_id: &str,
    requester_did: Option<&str>,
    agent_id: &str,
    opening: Option<&str>,
) -> Result<Option<gents_protocol::session::SessionCreationReceipt>> {
    use crate::identity::NodeIdentity;
    use gents_protocol::session::{SessionCreationCause, SessionCreationReceipt};
    let Some(opening) = opening else {
        return Ok(None);
    };
    let response = txn
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(opening),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let Some(rows) = response["data"]["AgentRequest"].as_array() else {
        return Ok(None);
    };
    let [row] = rows.as_slice() else {
        return Ok(None);
    };
    let Ok(request) = serde_json::from_value::<AgentRequestRow>(row.clone()) else {
        return Ok(None);
    };
    if request.node_did.as_deref() != Some(node_did)
        || request.session_id.as_deref() != Some(session_id)
        || request.requester_did.as_deref() != requester_did
        || request.agent_id.as_deref() != Some(agent_id)
        || crate::request_admission::verify_request_receipt_signature(&request).is_err()
    {
        return Ok(None);
    }
    let intent = request
        .input
        .as_ref()
        .and_then(|input| input.session_creation);
    let (cause, cause_binding) =
        if let (Some(intent), Some(call_doc), Some(_parent_doc), Some(_call_id)) = (
            intent,
            request.caused_by_parent_tool_call_doc_id.as_deref(),
            request.caused_by_parent_request_doc_id.as_deref(),
            request.caused_by_parent_tool_call_id.as_deref(),
        ) {
            match intent {
                gents_protocol::request_input::SessionCreationIntent::AgentNew => {
                    (SessionCreationCause::AgentNew, call_doc.to_owned())
                }
                gents_protocol::request_input::SessionCreationIntent::PluginJob => {
                    let Some(key) = request.retry_key.as_deref() else {
                        return Ok(None);
                    };
                    if crate::plugin_job::keys::decode_job_retry_key(key).is_none() {
                        return Ok(None);
                    }
                    (
                        SessionCreationCause::PluginJob,
                        serde_json::to_string(&(call_doc, key))?,
                    )
                }
            }
        } else if request.caused_by_parent_request_doc_id.is_some()
            && request.caused_by_parent_tool_call_doc_id.is_some()
        {
            if let Some(binding) = agent_new_binding_in_txn(txn, &request, opening).await? {
                (SessionCreationCause::AgentNew, binding)
            } else if let Some((binding, _)) = plugin_job_creation_binding(txn, &request).await? {
                (SessionCreationCause::PluginJob, binding)
            } else {
                return Ok(None);
            }
        } else if matches!(
            request.caused_by_trigger_kind.as_deref(),
            Some("event" | "schedule")
        ) && request.caused_by_trigger_id.is_some()
        {
            let Some(binding) = task_creation_binding(txn, &request).await? else {
                return Ok(None);
            };
            (SessionCreationCause::TaskTrigger, binding)
        } else {
            return Ok(None);
        };
    let Ok(identity) = crate::identity::RegisteredIdentity::from_registered_did(node_did, None)
    else {
        return Ok(None);
    };
    let mut receipt = SessionCreationReceipt {
        node_did: node_did.into(),
        session_id: session_id.into(),
        session_doc_id: session_doc_id.into(),
        requester_did: requester_did.map(str::to_owned),
        agent_id: agent_id.into(),
        opening_request_doc_id: opening.into(),
        cause,
        cause_binding,
        starter: request
            .input
            .as_ref()
            .and_then(|input| input.started_by.as_ref())
            .and_then(|source| {
                let parent = request.caused_by_parent_request_doc_id.as_ref()?;
                (request.requester_did.as_deref() == Some(source.node_did.as_str())
                    && request.admission_signer_did.as_deref() == Some(source.node_did.as_str())
                    && request.caused_by_parent_tool_call_doc_id.is_some()
                    && !source.session_id.is_empty())
                .then(|| gents_protocol::session::SessionCreationStarter {
                    source: source.clone(),
                    parent_request_doc_id: parent.clone(),
                })
            }),
        signature: Vec::new(),
    };
    receipt.signature = identity.sign(&receipt.signing_payload()).await?;
    Ok(Some(receipt))
}

fn verified_creation_receipt<'a>(
    session: &'a AgentSession,
    session_doc_id: &str,
    cause: gents_protocol::session::SessionCreationCause,
) -> Option<&'a gents_protocol::session::SessionCreationReceipt> {
    let receipt = session.provenance.as_ref()?.creation_receipt.as_ref()?;
    if receipt.node_did != session.node_did
        || receipt.session_id != session.session_id
        || receipt.session_doc_id != session_doc_id
        || receipt.requester_did != session.requester_did
        || receipt.agent_id != session.agent_id
        || receipt.cause != cause
        || !crate::identity::verify_did_signature(
            &session.node_did,
            &receipt.signing_payload(),
            &receipt.signature,
        )
        .ok()?
    {
        return None;
    }
    Some(receipt)
}

pub async fn verified_agent_new_creation(
    access: &ConfigAccess,
    target: &SessionScope,
    caller: &SessionScope,
) -> Result<bool> {
    use gents_protocol::session::SessionCreationCause;
    let own = session_rows(
        access,
        &session_scope_filter(
            &target.node_did,
            &target.session_id,
            target.requester_did.as_deref(),
        ),
    )
    .await?;
    let [owner] = own.as_slice() else {
        return Ok(false);
    };
    let session = &owner.session;
    let receipt = verified_creation_receipt(session, &owner.doc_id, SessionCreationCause::AgentNew)
        .or_else(|| {
            verified_creation_receipt(session, &owner.doc_id, SessionCreationCause::PluginJob)
        });
    let Some(receipt) = receipt else {
        return Ok(false);
    };
    let opening = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(&receipt.opening_request_doc_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await?;
    let [row] = opening.as_slice() else {
        return Ok(false);
    };
    let Ok(request) = serde_json::from_value::<AgentRequestRow>(row.clone()) else {
        return Ok(false);
    };
    if request.node_did.as_deref() != Some(target.node_did.as_str())
        || request.session_id.as_deref() != Some(target.session_id.as_str())
        || request.requester_did != target.requester_did
        || request.agent_id.as_deref() != Some(session.agent_id.as_str())
        || request.requester_did.as_deref() != Some(caller.node_did.as_str())
        || request.admission_signer_did.as_deref() != Some(caller.node_did.as_str())
        || crate::request_admission::verify_request_receipt_signature(&request).is_err()
    {
        return Ok(false);
    }
    if receipt.cause == SessionCreationCause::PluginJob {
        let source_bound = request
            .input
            .as_ref()
            .and_then(|input| input.session_creation)
            == Some(gents_protocol::request_input::SessionCreationIntent::PluginJob);
        let binding = if source_bound {
            match (
                request.caused_by_parent_tool_call_doc_id.as_deref(),
                request.retry_key.as_deref(),
            ) {
                (Some(call), Some(key)) => Some(serde_json::to_string(&(call, key))?),
                _ => None,
            }
        } else {
            None
        };
        return Ok(plugin_job_creation_binding(access, &request)
            .await?
            .is_some_and(|(legacy_binding, scope)| {
                scope == *caller
                    && binding.as_deref().unwrap_or(&legacy_binding) == receipt.cause_binding
            }));
    }
    let (Some(parent_doc), Some(call_doc), Some(call_id)) = (
        request.caused_by_parent_request_doc_id.as_deref(),
        request.caused_by_parent_tool_call_doc_id.as_deref(),
        request.caused_by_parent_tool_call_id.as_deref(),
    ) else {
        return Ok(false);
    };
    let parent = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(parent_doc),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await?;
    let [row] = parent.as_slice() else {
        return Ok(false);
    };
    let Ok(parent) = serde_json::from_value::<AgentRequestRow>(row.clone()) else {
        return Ok(false);
    };
    if parent.node_did.as_deref() != Some(caller.node_did.as_str())
        || parent.session_id.as_deref() != Some(caller.session_id.as_str())
        || parent.requester_did != caller.requester_did
        || request.caused_by_parent_request_id.as_deref() != Some(parent.request_id.as_str())
        || crate::request_admission::verify_request_receipt_signature(&parent).is_err()
    {
        return Ok(false);
    }
    let calls = collection(access, &format!(
        "{{AgentToolCall(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{request_doc_id node_did session_id requester_did tool_call_id tool_name}}}}",
        escape_graphql_string(call_doc),
    ), "AgentToolCall").await?;
    let [call] = calls.as_slice() else {
        return Ok(false);
    };
    if call["request_doc_id"].as_str() != Some(parent_doc)
        || call["node_did"].as_str() != Some(caller.node_did.as_str())
        || call["session_id"].as_str() != Some(caller.session_id.as_str())
        || call["requester_did"].as_str() != caller.requester_did.as_deref()
        || call["tool_call_id"].as_str() != Some(call_id)
        || call["tool_name"].as_str() != Some(crate::toolset::AGENT_NEW_TOOL_NAME)
    {
        return Ok(false);
    }
    if receipt.cause_binding != call_doc {
        return Ok(false);
    }
    let Ok(read) = crate::tool_call_lifecycle::query::load_tool_call_read(
        access,
        call_doc,
        &caller.node_did,
        &caller.session_id,
        caller.requester_did.as_deref(),
    )
    .await
    else {
        return Ok(false);
    };
    Ok(deferred_creation_allowed(
        true,
        true,
        true,
        agent_receipt_matches(&read, &request, &receipt.opening_request_doc_id, call_id),
    ))
}

fn deferred_creation_allowed(kind: bool, receipt: bool, opening: bool, cause: bool) -> bool {
    kind && receipt && opening && cause
}

#[cfg(test)]
mod creation_receipt_conformance {
    use super::*;
    use crate::identity::NodeIdentity;
    use gents_protocol::session::{SessionCreationCause, SessionCreationReceipt};

    #[test]
    fn job_creation_binding_matches_executable_model() {
        for case in &crate::lean_vocab_test::lean_causal_hop_contract().job_creation_cases {
            assert_eq!(
                super::job_creation_matches(case.parent, case.job, case.key, case.spawn),
                case.expected
            );
        }
    }

    #[test]
    fn deferred_creation_matches_executable_model() {
        for case in &crate::lean_vocab_test::lean_causal_hop_contract().deferred_creation_cases {
            assert_eq!(
                super::deferred_creation_allowed(case.kind, case.receipt, case.opening, case.cause),
                case.expected
            );
        }
    }

    #[tokio::test]
    async fn receipt_verification_matches_executable_creation_model() {
        let directory = tempfile::tempdir().unwrap();
        let identity =
            crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        let did = identity.did();
        for case in &crate::lean_vocab_test::lean_causal_hop_contract().creation_cases {
            let mut receipt = SessionCreationReceipt {
                node_did: did.into(),
                session_id: if case.scope_matches {
                    "session"
                } else {
                    "different"
                }
                .into(),
                requester_did: Some(did.into()),
                session_doc_id: if case.physical_matches {
                    "physical"
                } else {
                    "recreated"
                }
                .into(),
                agent_id: "agent".into(),
                opening_request_doc_id: "opening".into(),
                cause: if case.cause_matches {
                    SessionCreationCause::AgentNew
                } else {
                    SessionCreationCause::TaskTrigger
                },
                cause_binding: "binding".into(),
                starter: None,
                signature: Vec::new(),
            };
            receipt.signature = identity.sign(&receipt.signing_payload()).await.unwrap();
            if !case.signature_valid {
                receipt.signature[0] ^= 1;
            }
            let session: AgentSession = serde_json::from_value(serde_json::json!({
                "node_did": did, "session_id": "session", "requester_did": did,
                "agent_id": "agent", "created_at": "2026-01-01T00:00:00Z",
                "provenance": {"creation_receipt": receipt}
            }))
            .unwrap();
            assert_eq!(
                verified_creation_receipt(&session, "physical", SessionCreationCause::AgentNew)
                    .is_some(),
                case.expected,
                "{}",
                case.name
            );
        }
    }
}

fn agent_receipt_matches(
    read: &crate::tool_call_lifecycle::query::CanonicalToolCallRead,
    request: &AgentRequestRow,
    opening: &str,
    call_id: &str,
) -> bool {
    if read.tool_name != crate::toolset::AGENT_NEW_TOOL_NAME
        || read.request_doc_id
            != request
                .caused_by_parent_request_doc_id
                .as_deref()
                .unwrap_or_default()
    {
        return false;
    }
    let Some(result) = &read.result else {
        return false;
    };
    let Ok(text) = crate::tool_call_lifecycle::render_tool_result(result) else {
        return false;
    };
    let Ok(receipt) = serde_json::from_str::<crate::session_message::SessionMessageReceipt>(&text)
    else {
        return false;
    };
    receipt.ok
        && receipt.request_doc_id == opening
        && receipt.request_id == request.request_id
        && Some(receipt.session_id.as_str()) == request.session_id.as_deref()
        && receipt.tool_call_id == call_id
}

fn job_creation_matches(parent: bool, job: bool, key: bool, spawn: bool) -> bool {
    parent && job && key && spawn
}

async fn plugin_job_creation_binding<R>(
    access: &R,
    request: &AgentRequestRow,
) -> Result<Option<(String, SessionScope)>>
where
    R: ConfigRead + ?Sized,
{
    let (Some(parent_doc), Some(call_doc), Some(call_id), Some(key)) = (
        request.caused_by_parent_request_doc_id.as_deref(),
        request.caused_by_parent_tool_call_doc_id.as_deref(),
        request.caused_by_parent_tool_call_id.as_deref(),
        request.retry_key.as_deref(),
    ) else {
        return Ok(None);
    };
    let Some((slot, attempt)) = crate::plugin_job::keys::decode_job_retry_key(key) else {
        return Ok(None);
    };
    let parents = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(parent_doc),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await?;
    let [parent] = parents.as_slice() else {
        return Ok(None);
    };
    let Ok(parent) = serde_json::from_value::<AgentRequestRow>(parent.clone()) else {
        return Ok(None);
    };
    let (Some(node), Some(session)) = (parent.node_did.as_deref(), parent.session_id.as_deref())
    else {
        return Ok(None);
    };
    if request.requester_did.as_deref() != Some(node)
        || request.admission_signer_did.as_deref() != Some(node)
        || request.caused_by_parent_request_id.as_deref() != Some(parent.request_id.as_str())
        || crate::request_admission::verify_request_receipt_signature(&parent).is_err()
    {
        return Ok(None);
    }
    let calls = collection(access, &format!(
        "{{AgentToolCall(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{request_doc_id node_did session_id requester_did tool_call_id job_pin}}}}",
        escape_graphql_string(call_doc),
    ), "AgentToolCall").await?;
    let [call] = calls.as_slice() else {
        return Ok(None);
    };
    let Ok(pin) = serde_json::from_value::<gents_protocol::job::JobPin>(call["job_pin"].clone())
    else {
        return Ok(None);
    };
    let exact_job = call["request_doc_id"].as_str() == Some(parent_doc)
        && call["node_did"].as_str() == Some(node)
        && call["session_id"].as_str() == Some(session)
        && call["requester_did"].as_str() == parent.requester_did.as_deref()
        && call["tool_call_id"].as_str() == Some(call_id);
    let exact_key =
        key == crate::plugin_job::keys::job_spawn_key(&pin.chain_root_doc_id, &slot, attempt);
    let exact_spawn = request.session_id.as_deref()
        == Some(
            crate::plugin_job::keys::job_session_id(&pin.chain_root_doc_id, &slot, attempt)
                .as_str(),
        );
    if !job_creation_matches(true, exact_job, exact_key, exact_spawn) {
        return Ok(None);
    }
    let binding = serde_json::to_string(&(call_doc, &pin.chain_root_doc_id, slot, attempt, key))?;
    Ok(Some((
        binding,
        SessionScope {
            node_did: node.to_owned(),
            session_id: session.to_owned(),
            requester_did: parent.requester_did,
        },
    )))
}

async fn agent_new_binding_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequestRow,
    opening: &str,
) -> Result<Option<String>> {
    let (Some(parent_doc), Some(call_doc), Some(call_id)) = (
        request.caused_by_parent_request_doc_id.as_deref(),
        request.caused_by_parent_tool_call_doc_id.as_deref(),
        request.caused_by_parent_tool_call_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let parents = collection(
        txn,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{{}}}}}",
            escape_graphql_string(parent_doc),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await?;
    let [row] = parents.as_slice() else {
        return Ok(None);
    };
    let Ok(parent) = serde_json::from_value::<AgentRequestRow>(row.clone()) else {
        return Ok(None);
    };
    let (Some(node), Some(session)) = (parent.node_did.as_deref(), parent.session_id.as_deref())
    else {
        return Ok(None);
    };
    if request.requester_did.as_deref() != Some(node)
        || request.admission_signer_did.as_deref() != Some(node)
        || request.caused_by_parent_request_id.as_deref() != Some(parent.request_id.as_str())
        || crate::request_admission::verify_request_receipt_signature(&parent).is_err()
    {
        return Ok(None);
    }
    let Ok(read) = crate::tool_call_lifecycle::query::load_tool_call_read_in_txn(
        txn,
        call_doc,
        node,
        session,
        parent.requester_did.as_deref(),
    )
    .await
    else {
        return Ok(None);
    };
    if agent_receipt_matches(&read, request, opening, call_id) {
        Ok(Some(call_doc.to_owned()))
    } else {
        Ok(None)
    }
}

async fn task_creation_binding<R>(access: &R, request: &AgentRequestRow) -> Result<Option<String>>
where
    R: ConfigRead + ?Sized,
{
    let (Some(node), Some(session), Some(trigger)) = (
        request.node_did.as_deref(),
        request.session_id.as_deref(),
        request.caused_by_trigger_id.as_deref(),
    ) else {
        return Ok(None);
    };
    if request.admission_kind.as_deref() != Some("runtime-internal")
        || request.admission_signer_did.as_deref() != Some(node)
        || request.runtime_issuer_did.as_deref() != Some(node)
        || request.runtime_source_kind.as_deref() != Some("automated-trigger")
        || request.runtime_source_request_id.as_deref() != Some(trigger)
        || !matches!(
            request.caused_by_trigger_kind.as_deref(),
            Some("event" | "schedule")
        )
    {
        return Ok(None);
    }
    let fires = collection(access, &format!(
        "{{TriggerFire(filter: {{owner_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 2) {{fire_key trigger_id task_id session_id source_collection source_doc_id}}}}",
        escape_graphql_string(node), escape_graphql_string(&request.request_id),
    ), "TriggerFire").await?;
    let [fire] = fires.as_slice() else {
        return Ok(None);
    };
    let (Some(key), Some(task)) = (
        fire["fire_key"].as_str().filter(|v| !v.is_empty()),
        fire["task_id"].as_str().filter(|v| !v.is_empty()),
    ) else {
        return Ok(None);
    };
    let (Some(source_collection), Some(source_doc_id)) = (
        fire["source_collection"].as_str(),
        fire["source_doc_id"].as_str(),
    ) else {
        return Ok(None);
    };
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: node.to_owned(),
        trigger_id: trigger.to_owned(),
        source_collection: source_collection.to_owned(),
        source_doc_id: source_doc_id.to_owned(),
    };
    if fire["trigger_id"].as_str() != Some(trigger)
        || fire["session_id"].as_str() != Some(session)
        || key != identity.fire_key()
        || request.request_id != identity.request_id()
    {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&(key, task))?))
}
