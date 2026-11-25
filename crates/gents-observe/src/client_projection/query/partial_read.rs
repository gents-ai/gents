use std::collections::BTreeSet;

use super::collection_roots::load_collection_roots_on;
use super::snapshot_loaders::{complete_access_snapshot_requests, selected_node_root};
use super::*;
use gents_documents::views::config::NodeList;
use gents_protocol::operations::session::SessionListInput;
use gents_protocol::session::SessionScope;
use gents_session::views::requests::RequestRows;
use gents_session::views::transcript::Scope;
use gents_session::views::SessionList;
use gents_store::config_client::ConfigAccess;

/// What one partial read of a node reads (`ObserverResync.startCollections`):
/// the self-contained roots and the session scopes queued since the last read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialRead {
    pub roots: BTreeSet<CollectionRoot>,
    /// Session scopes whose session row and requests are read again
    /// (`ObserverResync.Delivery.reloadSessionScope`).
    pub scopes: BTreeSet<SessionScope>,
}

impl PartialRead {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.scopes.is_empty()
    }

    pub fn extend(&mut self, other: Self) {
        self.roots.extend(other.roots);
        self.scopes.extend(other.scopes);
    }
}

/// The session row and the requests the node snapshot shows for `scope`,
/// read from the scope alone (`SessionRequestReload.ShownFromScope`): its
/// session, its nonterminal requests, the session's head and every row those
/// need, through the snapshot's own derivation. The Node root is read beside
/// them, so a Node this client cannot observe is a refusal, not a missing
/// session.
async fn load_scope(
    access: &ConfigAccess,
    node_did: &str,
    scope: &SessionScope,
) -> std::result::Result<(Vec<AgentSession>, Vec<AgentRequestRow>), SnapshotReadError> {
    let (nodes, sessions, seeds) = access
        .read_views(&(
            NodeList::new(node_did),
            SessionList {
                input: SessionListInput {
                    node_did: Some(scope.node_did.clone()),
                    session_id: Some(scope.session_id.clone()),
                    requester_did: scope.requester_did.clone(),
                    include_archived: true,
                },
            },
            RequestRows::new(Scope::Session(scope.clone())).nonterminal(),
        ))
        .await
        .map_err(anyhow::Error::from)?;
    selected_node_root(!nodes.is_empty())?;
    let sessions = sessions
        .into_iter()
        .map(|row| row.session)
        .filter(|session| session.requester_did == scope.requester_did)
        .collect::<Vec<_>>();
    let requests =
        complete_access_snapshot_requests(access, node_did, &sessions, &[], seeds).await?;
    Ok((sessions, requests))
}

/// The rows of `read` alone, as the node snapshot reads them.
pub async fn load_partial_read_on(
    access: &ConfigAccess,
    node_did: &str,
    read: &PartialRead,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    let mut rows = load_collection_roots_on(access, node_did, &read.roots)
        .await?
        .to_rows();
    for scope in &read.scopes {
        let (sessions, requests) = load_scope(access, node_did, scope).await?;
        rows.sessions.extend(sessions);
        rows.requests.extend(requests);
    }
    Ok(ClientStore::from_rows(rows))
}

/// `current` with the rows of `read`'s roots and the session rows and
/// requests of its scopes replaced by `incoming`'s, every other row kept.
pub fn replace_partial_read(
    current: &ClientStore,
    read: &PartialRead,
    incoming: ClientStore,
) -> ClientStore {
    let (sessions, requests) = (incoming.sessions.clone(), incoming.requests.clone());
    let replaced = replace_collection_roots(current, &read.roots, incoming);
    if read.scopes.is_empty() {
        replaced
    } else {
        replaced.replace_session_scopes(&read.scopes, sessions, requests)
    }
}
