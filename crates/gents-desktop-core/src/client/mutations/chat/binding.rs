use anyhow::{bail, Result};

use crate::client::store::ClientStore;

use super::super::graphql::normalize_optional_string;

pub(super) struct ResolvedAgentBinding {
    pub(super) agent_id: Option<String>,
}

pub(super) fn resolve_agent_binding(
    store: &ClientStore,
    node_did: &str,
    requested_agent_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<ResolvedAgentBinding> {
    let existing_session = session_id.and_then(|session_id| {
        store
            .sessions
            .iter()
            .enumerate()
            .find(|(index, row)| {
                row.session_id == session_id
                    && row.node_did == node_did
                    && store
                        .session_source_node_dids
                        .get(*index)
                        .and_then(|source| source.as_deref())
                        .is_none_or(|source| source == node_did)
            })
            .map(|(_index, row)| row)
    });

    let agent_id = resolve_agent_id(
        store,
        node_did,
        requested_agent_id,
        existing_session.map(|row| row.agent_id.as_str()),
    )?;
    Ok(ResolvedAgentBinding { agent_id })
}

fn resolve_agent_id(
    store: &ClientStore,
    node_did: &str,
    requested_agent_id: Option<&str>,
    existing_session_agent_id: Option<&str>,
) -> Result<Option<String>> {
    let requested = normalize_optional_string(requested_agent_id);

    let session_agent = normalize_optional_string(existing_session_agent_id);

    if let (Some(existing), Some(requested)) = (session_agent, requested) {
        if existing != requested {
            bail!("AgentSession session agent mismatch: existing={existing} requested={requested}");
        }
    }

    let resolved = session_agent
        .or(requested)
        .or_else(|| {
            store
                .nodes
                .iter()
                .find(|row| row.node_did == node_did)
                .and_then(|row| normalize_optional_string(row.default_agent_id.as_deref()))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no agent is bound to this session and Node {node_did} has no default_agent_id"
            )
        })?
        .to_owned();

    Ok(normalize_optional_string(Some(&resolved)).map(ToOwned::to_owned))
}
