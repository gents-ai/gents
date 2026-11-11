use std::cmp::Reverse;

use rapidhash::{HashMapExt, RapidHashMap};

use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::AgentSession;

pub type SessionOrderKey = (
    Reverse<Option<String>>,
    Reverse<Option<String>>,
    String,
    Option<String>,
);

/// Session lists show the latest activity first, then the newest session.
pub fn session_order_key(
    updated_at: Option<&str>,
    created_at: Option<&str>,
    session_id: &str,
    requester_did: Option<&str>,
) -> SessionOrderKey {
    (
        Reverse(updated_at.map(str::to_owned)),
        Reverse(created_at.map(str::to_owned)),
        session_id.to_owned(),
        requester_did.map(str::to_owned),
    )
}

/// One of a node's sessions with the request its list row shows.
#[derive(Debug, Clone, Copy)]
pub struct SessionSummaryEntry<'a> {
    pub session: &'a AgentSession,
    /// The session's indexed latest request, when exactly one observed row is
    /// it, as the session displays it.
    pub request: Option<&'a AgentRequestRow>,
    /// The request whose trigger caused the session: its unique opening
    /// request, else the displayed or the indexed one.
    pub trigger_request: Option<&'a AgentRequestRow>,
    pub lifecycle: Option<RequestLifecycleState>,
    pub turn_state: Option<ClientTurnState>,
    pub latest_request_doc_id: Option<&'a str>,
    pub latest_request_id: Option<&'a str>,
}

/// `node_did`'s sessions in [`session_order_key`] order.
pub fn session_summary_entries<'a>(
    sessions: &'a [AgentSession],
    requests: &'a [AgentRequestRow],
    node_did: &str,
) -> Vec<SessionSummaryEntry<'a>> {
    let mut requests_by_session: RapidHashMap<(&str, Option<&str>), Vec<&AgentRequestRow>> =
        RapidHashMap::new();
    for request in requests {
        if request.node_did.as_deref() != Some(node_did) {
            continue;
        }
        if let Some(session_id) = request.session_id.as_deref() {
            requests_by_session
                .entry((session_id, request.requester_did.as_deref()))
                .or_default()
                .push(request);
        }
    }
    let mut entries = sessions
        .iter()
        .filter(|session| session.node_did == node_did)
        .map(|session| {
            let indexed = session
                .observation
                .as_ref()
                .and_then(|value| value.latest_request.as_ref());
            let session_requests = requests_by_session
                .get(&(
                    session.session_id.as_str(),
                    session.requester_did.as_deref(),
                ))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut exact_requests = session_requests.iter().copied().filter(|request| {
                indexed.is_some_and(|indexed| {
                    request.doc_id.as_deref() == Some(indexed.request_doc_id.as_str())
                        && request.request_id == indexed.request_id
                })
            });
            let candidate = exact_requests.next();
            let exact = if exact_requests.next().is_none() {
                candidate
            } else {
                None
            };
            let request =
                exact.map(|request| super::session_display_request(session_requests, request));
            let lifecycle = request
                .and_then(|request| request.lifecycle_state)
                .or_else(|| indexed.map(|indexed| indexed.lifecycle_state));
            let opening_doc = session
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.opening_request_doc_id.as_deref());
            let mut openings = session_requests.iter().copied().filter(|candidate| {
                opening_doc.is_some()
                    && candidate.doc_id.as_deref() == opening_doc
                    && candidate.agent_did.as_deref() == Some(session.agent_did.as_str())
            });
            let opening = openings.next().filter(|_| openings.next().is_none());
            let trigger_request = opening
                .filter(|request| request.caused_by_trigger_kind.is_some())
                .or_else(|| request.filter(|request| request.caused_by_trigger_kind.is_some()))
                .or_else(|| exact.filter(|request| request.caused_by_trigger_kind.is_some()));
            SessionSummaryEntry {
                session,
                request,
                trigger_request,
                lifecycle,
                turn_state: lifecycle.and_then(|state| {
                    gents_protocol::client_protocol::derive_persisted_attempt(
                        state.as_str(),
                        request.is_some_and(|request| request.superseded_by_request.is_some()),
                    )
                }),
                latest_request_doc_id: request
                    .and_then(|request| request.doc_id.as_deref())
                    .or_else(|| indexed.map(|indexed| indexed.request_doc_id.as_str())),
                latest_request_id: request
                    .map(|request| request.request_id.as_str())
                    .or_else(|| indexed.map(|indexed| indexed.request_id.as_str())),
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_cached_key(|entry| {
        session_order_key(
            entry
                .session
                .observation
                .as_ref()
                .map(|value| value.last_activity_at.as_str()),
            Some(&entry.session.created_at),
            &entry.session.session_id,
            entry.session.requester_did.as_deref(),
        )
    });
    entries
}
