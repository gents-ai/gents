use std::collections::HashSet;

use crate::client::{ClientPeerStatus, ClientStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEntry {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub agent_label: String,
    pub addr: String,
    pub connected: bool,
    pub warning: Option<String>,
}

pub fn build_deployment_entries(
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> Vec<DeploymentEntry> {
    let mut entries: Vec<_> = peer_statuses
        .iter()
        .map(|status| DeploymentEntry {
            peer_id: status.peer_id.clone(),
            label: status.label.clone(),
            agent_did: status.agent_did.clone(),
            agent_label: display_name_for_agent(store, &status.agent_did),
            addr: abbreviate_address(&status.addr),
            connected: status.dial_succeeded,
            warning: status.last_error.clone(),
        })
        .collect();

    let mut seen_agents: HashSet<String> = entries
        .iter()
        .map(|entry| entry.agent_did.clone())
        .collect();

    for principal in &store.agent_principals {
        if !seen_agents.insert(principal.agent_did.clone()) {
            continue;
        }

        entries.push(DeploymentEntry {
            peer_id: format!("local:{}", principal.agent_did),
            label: "Local Replica".to_string(),
            agent_did: principal.agent_did.clone(),
            agent_label: display_name_for_agent(store, &principal.agent_did),
            addr: "local replica".to_string(),
            connected: true,
            warning: None,
        });
    }

    entries.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
    entries
}

fn display_name_for_agent(store: &ClientStore, agent_did: &str) -> String {
    store
        .agent_principals
        .iter()
        .find(|row| row.agent_did == agent_did)
        .and_then(|row| row.display_name.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            agent_did
                .rsplit(':')
                .next()
                .filter(|segment| !segment.trim().is_empty())
                .unwrap_or(agent_did)
                .to_string()
        })
}

fn abbreviate_address(value: &str) -> String {
    if value.len() <= 18 {
        return value.to_string();
    }

    format!("{}..{}", &value[..10], &value[value.len() - 4..])
}
