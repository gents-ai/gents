mod principal;
mod behavior;
mod inference_profile;
mod tool_selection;
mod serde_helpers;
mod graphql_fields;

pub use principal::{load_agent_principal, upsert_agent_principal, AgentPrincipal};
pub(crate) use principal::{load_agent_principal_by_doc_id, load_agent_principal_record};

pub use behavior::{
    list_agent_behaviors, load_agent_behavior, upsert_agent_behavior, AgentBehavior,
};
#[allow(unused_imports)]
pub(crate) use behavior::{
    list_agent_behavior_records, load_agent_behavior_by_doc_id, load_agent_behavior_record,
};
use behavior::create_default_behavior;

pub use inference_profile::{load_inference_profile, upsert_inference_profile, InferenceProfile};
#[allow(unused_imports)]
pub(crate) use inference_profile::{
    list_inference_profile_records, load_inference_profile_by_doc_id,
    load_inference_profile_record,
};

pub use tool_selection::{load_tool_selection, upsert_tool_selection, ToolSelectionDocument};
#[allow(unused_imports)]
pub(crate) use tool_selection::{
    list_all_tool_selection_records, list_tool_selection_records,
    load_tool_selection_by_doc_id, load_tool_selection_record,
};

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

#[derive(Debug, Clone, PartialEq)]
pub struct PrincipalBootstrap {
    pub principal: AgentPrincipal,
    pub default_behavior: AgentBehavior,
    pub created_principal: bool,
    pub created_default_behavior: bool,
}

pub fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

pub async fn ensure_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<PrincipalBootstrap> {
    let existing_principal = load_agent_principal(node, agent_did).await?;
    let (default_behavior_id, created_principal) = match existing_principal.as_ref() {
        Some(principal) => {
            let behavior_id = serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
            (behavior_id, false)
        }
        None => (default_behavior_id_for_agent(agent_did), true),
    };

    let (default_behavior, created_default_behavior) = match load_agent_behavior(
        node,
        &default_behavior_id,
    )
    .await?
    {
        Some(behavior) => {
            if behavior.agent_did != agent_did {
                return Err(anyhow!(
                    "AgentBehavior {default_behavior_id} belongs to {} not {agent_did}",
                    behavior.agent_did
                ));
            }
            (behavior, false)
        }
        None => {
            if existing_principal
                .as_ref()
                .and_then(|principal| {
                    serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref())
                })
                .is_some()
            {
                return Err(anyhow!(
                    "AgentPrincipal {agent_did} references missing default behavior {default_behavior_id}"
                ));
            }

            create_default_behavior(node, agent_did, &default_behavior_id).await?;
            let behavior = load_agent_behavior(node, &default_behavior_id)
                .await?
                .ok_or_else(|| {
                    anyhow!("default behavior {default_behavior_id} was not persisted")
                })?;
            (behavior, true)
        }
    };

    match existing_principal {
        Some(principal) => {
            if serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref()).is_none() {
                let fallback_display_name = serde_helpers::default_display_name_for_did(agent_did);
                upsert_agent_principal(
                    node,
                    agent_did,
                    principal
                        .display_name
                        .as_deref()
                        .or(Some(fallback_display_name.as_str())),
                    Some(&default_behavior_id),
                    principal.enabled,
                )
                .await?;
            }
        }
        None => {
            let fallback_display_name = serde_helpers::default_display_name_for_did(agent_did);
            upsert_agent_principal(
                node,
                agent_did,
                Some(fallback_display_name.as_str()),
                Some(&default_behavior_id),
                true,
            )
            .await?;
        }
    }

    let principal = load_agent_principal(node, agent_did)
        .await?
        .ok_or_else(|| anyhow!("AgentPrincipal {agent_did} was not persisted"))?;

    Ok(PrincipalBootstrap {
        principal,
        default_behavior,
        created_principal,
        created_default_behavior,
    })
}

#[cfg(test)]
mod tests;
