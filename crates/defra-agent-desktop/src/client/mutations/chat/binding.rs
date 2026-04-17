use anyhow::{bail, Result};

use crate::client::store::ClientStore;

use super::super::graphql::normalize_optional_string;

pub(super) struct ResolvedAgentBinding {
    pub(super) agent_name: String,
    pub(super) behavior_id: Option<String>,
}

pub(super) fn resolve_agent_binding(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<ResolvedAgentBinding> {
    let existing_conversation = session_id.and_then(|session_id| {
        store
            .conversations
            .iter()
            .find(|row| row.session_id == session_id)
    });
    let existing_session = session_id.and_then(|session_id| {
        store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id)
    });

    let behavior_id = resolve_behavior_id(
        store,
        agent_did,
        requested_behavior_id,
        existing_conversation.and_then(|row| row.behavior_id.as_deref()),
        existing_session.and_then(|row| row.behavior_id.as_deref()),
    )?;
    let agent_name = existing_conversation
        .and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        .or_else(|| {
            existing_session.and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        })
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.display_name.as_deref()))
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_display_name_for_did(agent_did));

    Ok(ResolvedAgentBinding {
        agent_name,
        behavior_id,
    })
}

fn resolve_behavior_id(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    existing_conversation_behavior_id: Option<&str>,
    existing_session_behavior_id: Option<&str>,
) -> Result<Option<String>> {
    let requested = normalize_optional_string(requested_behavior_id);

    let conversation_behavior = normalize_optional_string(existing_conversation_behavior_id);
    let session_behavior = normalize_optional_string(existing_session_behavior_id);

    if let (Some(existing), Some(requested)) = (conversation_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentConversation session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    if let (Some(existing), Some(requested)) = (session_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentSession session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    let resolved = conversation_behavior
        .or(session_behavior)
        .or(requested)
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.default_behavior_id.as_deref()))
        })
        .or_else(|| {
            store
                .behaviors
                .iter()
                .find(|row| {
                    row.agent_did.as_deref() == Some(agent_did) && row.enabled != Some(false)
                })
                .map(|row| row.behavior_id.as_str())
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));

    Ok(normalize_optional_string(Some(&resolved)).map(ToOwned::to_owned))
}

pub(super) fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

fn default_display_name_for_did(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(agent_did)
        .to_string()
}
