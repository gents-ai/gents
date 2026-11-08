use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;
use rapidhash::RapidHashSet;

use crate::store::identity::NodeIdentity;

#[derive(Debug, thiserror::Error)]
#[error("active agent identity watch closed")]
pub(crate) struct AgentIdentityWatchClosed;

/// The runtime's current Agent signers as background owners observe them.
/// Each read observes one published snapshot; callers that need the watched
/// identities and the active Agent set together read them in one call.
pub(crate) trait AgentIdentityWatch: Send + Sync {
    fn watcher_identities(&self) -> Result<Vec<Arc<dyn NodeIdentity>>>;

    /// Watched identities and the DIDs of the active Agents, from the same
    /// snapshot.
    fn watched_and_active_agents(
        &self,
    ) -> Result<(Vec<Arc<dyn NodeIdentity>>, RapidHashSet<String>)>;

    fn changed(&mut self) -> BoxFuture<'_, Result<(), AgentIdentityWatchClosed>>;
}
