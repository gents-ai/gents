use gents_documents::views::agents::{AgentList, AgentRow};
use gents_documents::views::config::DefaultAgentSelectionList;
use gents_store::config_client::ConfigAccess;
use gents_store::views::ViewError;
use rapidhash::RapidHashMap;

use super::profiles::{ProfileList, ProfileRow};

pub struct AgentCatalogRow {
    pub agent: AgentRow,
    pub profile: Option<ProfileRow>,
}

pub struct AgentCatalog {
    pub agents: Vec<AgentCatalogRow>,
    pub profile_ids: Vec<String>,
    pub default_agent_did: Option<String>,
    pub total: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

pub async fn read_agent_catalog(
    access: &ConfigAccess,
    node_did: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<AgentCatalog, ViewError> {
    access
        .transact_readonly("view.agent_catalog", |txn| {
            Box::pin(async move {
                let agents = txn.read_view(&AgentList::new(node_did)).await?;
                let total = agents.len();
                let mut agents = agents
                    .into_iter()
                    .filter(|row| after.is_none_or(|after| row.agent.agent_did.as_str() > after))
                    .take(limit.saturating_add(1))
                    .collect::<Vec<_>>();
                let truncated = agents.len() > limit;
                agents.truncate(limit);
                let next_cursor = truncated
                    .then(|| agents.last().map(|row| row.agent.agent_did.clone()))
                    .flatten();
                let profiles = txn.read_view(&ProfileList::new(node_did)).await?;
                let default = txn
                    .read_view(&DefaultAgentSelectionList::new(node_did))
                    .await?;
                let default_agent_did = default
                    .into_iter()
                    .next()
                    .and_then(|row| row.document.default_agent_did)
                    .filter(|did| !did.trim().is_empty());
                let profile_ids = profiles
                    .iter()
                    .map(|row| row.profile.profile_id.clone())
                    .collect();
                let profiles: RapidHashMap<_, _> = profiles
                    .into_iter()
                    .map(|row| (row.profile.profile_id.clone(), row))
                    .collect();
                let agents = agents
                    .into_iter()
                    .map(|agent| AgentCatalogRow {
                        profile: profiles.get(&agent.agent.inference_profile_id).cloned(),
                        agent,
                    })
                    .collect();
                Ok(AgentCatalog {
                    agents,
                    profile_ids,
                    default_agent_did,
                    total,
                    truncated,
                    next_cursor,
                })
            })
        })
        .await
}
