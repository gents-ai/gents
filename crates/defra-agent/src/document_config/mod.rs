mod behavior;
mod event_trigger;
mod graphql_fields;
mod inference_profile;
mod principal;
mod schedule;
mod serde_helpers;
mod task;
mod tool_selection;

pub use principal::{load_agent_principal, upsert_agent_principal, AgentPrincipal};
pub(crate) use principal::{load_agent_principal_by_doc_id, load_agent_principal_record};

use behavior::create_default_behavior;
#[allow(unused_imports)]
pub(crate) use behavior::{
    list_agent_behavior_records, load_agent_behavior_by_doc_id, load_agent_behavior_record,
};
pub use behavior::{
    list_agent_behaviors, load_agent_behavior, upsert_agent_behavior, AgentBehavior,
};

pub use inference_profile::{
    default_inference_profile_id_for_behavior, load_inference_profile, upsert_inference_profile,
    InferenceProfile,
};
#[allow(unused_imports)]
pub(crate) use inference_profile::{
    list_inference_profile_records, load_inference_profile_by_doc_id, load_inference_profile_record,
};

pub use tool_selection::default_tool_selection_id_for_behavior;
#[allow(unused_imports)]
pub(crate) use tool_selection::{
    list_all_tool_selection_records, list_tool_selection_records, load_tool_selection_by_doc_id,
    load_tool_selection_record,
};
pub use tool_selection::{load_tool_selection, upsert_tool_selection, ToolSelectionDocument};

#[allow(unused_imports)]
pub(crate) use event_trigger::{
    list_event_trigger_records, load_event_trigger_by_doc_id, update_event_trigger_runtime_fields,
    EventTrigger, EventTriggerRuntimeUpdate,
};
#[allow(unused_imports)]
pub(crate) use schedule::{
    list_schedule_records, load_schedule_by_doc_id, load_schedule_next_run_at,
    update_schedule_runtime_fields, Schedule, ScheduleRuntimeUpdate,
};
#[allow(unused_imports)]
pub(crate) use task::{list_task_records, load_task_by_doc_id, Task};

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, PartialEq)]
pub struct PrincipalBootstrap {
    pub principal: AgentPrincipal,
    pub default_behavior: AgentBehavior,
    pub default_inference_profile: InferenceProfile,
    pub created_principal: bool,
    pub created_default_behavior: bool,
    pub created_default_inference_profile: bool,
}

pub const DEFAULT_BEHAVIOR_ID: &str = "default";

pub fn default_behavior_id_for_agent(_agent_did: &str) -> String {
    DEFAULT_BEHAVIOR_ID.to_string()
}

fn legacy_default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

pub async fn ensure_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<PrincipalBootstrap> {
    let existing_principal = load_agent_principal(node, agent_did).await?;
    let (default_behavior_id, created_principal, migrated_legacy_default_behavior_id) =
        match existing_principal.as_ref() {
            Some(principal) => {
                let behavior_id = serde_helpers::normalize_optional_string(
                    principal.default_behavior_id.as_deref(),
                )
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
                if behavior_id == legacy_default_behavior_id_for_agent(agent_did) {
                    (default_behavior_id_for_agent(agent_did), false, true)
                } else {
                    (behavior_id, false, false)
                }
            }
            None => (default_behavior_id_for_agent(agent_did), true, false),
        };

    let default_inference_profile_id =
        inference_profile::default_inference_profile_id_for_behavior(&default_behavior_id);

    let mut created_profile_with_default_behavior = false;
    let (mut default_behavior, created_default_behavior) = match load_agent_behavior(
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
            if !migrated_legacy_default_behavior_id
                && existing_principal
                    .as_ref()
                    .and_then(|principal| {
                        serde_helpers::normalize_optional_string(
                            principal.default_behavior_id.as_deref(),
                        )
                    })
                    .is_some()
            {
                return Err(anyhow!(
                    "AgentPrincipal {agent_did} references missing default behavior {default_behavior_id}"
                ));
            }

            let behavior = if migrated_legacy_default_behavior_id {
                let legacy_behavior_id = legacy_default_behavior_id_for_agent(agent_did);
                migrate_legacy_default_behavior(
                    node,
                    agent_did,
                    &legacy_behavior_id,
                    &default_behavior_id,
                )
                .await?
            } else {
                None
            };

            match behavior {
                Some(behavior) => (behavior, true),
                None => {
                    let profile = inference_profile::create_default_inference_profile(
                        node,
                        &default_behavior_id,
                    )
                    .await?;
                    created_profile_with_default_behavior = true;
                    create_default_behavior(
                        node,
                        agent_did,
                        &default_behavior_id,
                        &profile.profile_id,
                    )
                    .await?;
                    let behavior = load_agent_behavior(node, &default_behavior_id)
                        .await?
                        .ok_or_else(|| {
                            anyhow!("default behavior {default_behavior_id} was not persisted")
                        })?;
                    (behavior, true)
                }
            }
        }
    };

    let (default_inference_profile, created_default_inference_profile) =
        match load_inference_profile(node, &default_inference_profile_id).await? {
            Some(profile) => (profile, created_profile_with_default_behavior),
            None => (
                inference_profile::create_default_inference_profile(node, &default_behavior_id)
                    .await?,
                true,
            ),
        };

    if serde_helpers::normalize_optional_string(default_behavior.inference_profile_id.as_deref())
        .is_none()
    {
        default_behavior.inference_profile_id = Some(default_inference_profile.profile_id.clone());
        upsert_agent_behavior(node, &default_behavior).await?;
        default_behavior = load_agent_behavior(node, &default_behavior_id)
            .await?
            .ok_or_else(|| anyhow!("default behavior {default_behavior_id} was not persisted"))?;
    }

    match existing_principal {
        Some(principal) => {
            if migrated_legacy_default_behavior_id
                || serde_helpers::normalize_optional_string(
                    principal.default_behavior_id.as_deref(),
                )
                .is_none()
            {
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
        default_inference_profile,
        created_principal,
        created_default_behavior,
        created_default_inference_profile,
    })
}

async fn migrate_legacy_default_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    legacy_behavior_id: &str,
    default_behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    let Some(mut behavior) = load_agent_behavior(node, legacy_behavior_id).await? else {
        return Ok(None);
    };

    if behavior.agent_did != agent_did {
        return Err(anyhow!(
            "AgentBehavior {legacy_behavior_id} belongs to {} not {agent_did}",
            behavior.agent_did
        ));
    }

    migrate_legacy_default_profile(node, legacy_behavior_id, default_behavior_id, &mut behavior)
        .await?;
    migrate_legacy_default_tool_selection(
        node,
        legacy_behavior_id,
        default_behavior_id,
        &mut behavior,
    )
    .await?;

    behavior.behavior_id = default_behavior_id.to_string();
    upsert_agent_behavior(node, &behavior).await?;
    rewrite_legacy_behavior_references(node, legacy_behavior_id, default_behavior_id).await?;
    delete_legacy_default_documents(node, legacy_behavior_id).await?;
    load_agent_behavior(node, default_behavior_id).await
}

async fn migrate_legacy_default_profile(
    node: &EmbeddedNode,
    legacy_behavior_id: &str,
    default_behavior_id: &str,
    behavior: &mut AgentBehavior,
) -> Result<()> {
    let legacy_profile_id = format!("{legacy_behavior_id}:profile");
    let profile_id =
        serde_helpers::normalize_optional_string(behavior.inference_profile_id.as_deref());
    if profile_id != Some(legacy_profile_id.as_str()) {
        return Ok(());
    }

    let default_profile_id =
        inference_profile::default_inference_profile_id_for_behavior(default_behavior_id);
    if load_inference_profile(node, &default_profile_id)
        .await?
        .is_none()
    {
        if let Some(mut profile) = load_inference_profile(node, &legacy_profile_id).await? {
            profile.profile_id = default_profile_id.clone();
            upsert_inference_profile(node, &profile).await?;
        }
    }
    behavior.inference_profile_id = Some(default_profile_id);
    Ok(())
}

async fn migrate_legacy_default_tool_selection(
    node: &EmbeddedNode,
    legacy_behavior_id: &str,
    default_behavior_id: &str,
    behavior: &mut AgentBehavior,
) -> Result<()> {
    let legacy_selection_id = format!("{legacy_behavior_id}:tools");
    let selection_id =
        serde_helpers::normalize_optional_string(behavior.tool_selection_id.as_deref());
    if selection_id != Some(legacy_selection_id.as_str()) {
        return Ok(());
    }

    let default_selection_id =
        tool_selection::default_tool_selection_id_for_behavior(default_behavior_id);
    if load_tool_selection(node, &default_selection_id)
        .await?
        .is_none()
    {
        if let Some(mut selection) = load_tool_selection(node, &legacy_selection_id).await? {
            selection.selection_id = default_selection_id.clone();
            upsert_tool_selection(node, &selection).await?;
        }
    }
    behavior.tool_selection_id = Some(default_selection_id);
    Ok(())
}

async fn rewrite_legacy_behavior_references(
    node: &EmbeddedNode,
    legacy_behavior_id: &str,
    default_behavior_id: &str,
) -> Result<()> {
    for collection in [
        "Task",
        "AgentConversation",
        "AgentSession",
        "AgentRequest",
        "AgentResponse",
    ] {
        rewrite_string_field(
            node,
            collection,
            "behavior_id",
            legacy_behavior_id,
            "behavior_id",
            default_behavior_id,
        )
        .await?;
    }
    rewrite_string_field(
        node,
        "AgentRuntime",
        "default_behavior_id",
        legacy_behavior_id,
        "default_behavior_id",
        default_behavior_id,
    )
    .await
}

async fn rewrite_string_field(
    node: &EmbeddedNode,
    collection: &str,
    filter_field: &str,
    old_value: &str,
    input_field: &str,
    new_value: &str,
) -> Result<()> {
    let escaped_old_value = escape_graphql_string(old_value);
    let escaped_new_value = escape_graphql_string(new_value);
    let mutation = format!(
        r#"mutation {{
            update_{collection}(
                filter: {{ {filter_field}: {{ _eq: "{escaped_old_value}" }} }},
                input: {{ {input_field}: "{escaped_new_value}" }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "rewrite {collection}.{input_field} from {old_value} to {new_value} failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

async fn delete_legacy_default_documents(
    node: &EmbeddedNode,
    legacy_behavior_id: &str,
) -> Result<()> {
    if let Some((doc_id, _)) = load_agent_behavior_record(node, legacy_behavior_id).await? {
        delete_document_by_doc_id(node, "AgentBehavior", &doc_id).await?;
    }

    let legacy_profile_id = format!("{legacy_behavior_id}:profile");
    if let Some((doc_id, _)) = load_inference_profile_record(node, &legacy_profile_id).await? {
        delete_document_by_doc_id(node, "InferenceProfile", &doc_id).await?;
    }

    let legacy_selection_id = format!("{legacy_behavior_id}:tools");
    if let Some((doc_id, _)) = load_tool_selection_record(node, &legacy_selection_id).await? {
        delete_document_by_doc_id(node, "ToolSelection", &doc_id).await?;
    }

    Ok(())
}

async fn delete_document_by_doc_id(
    node: &EmbeddedNode,
    collection: &str,
    doc_id: &str,
) -> Result<()> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            delete_{collection}(docID: "{escaped_doc_id}") {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("delete {collection} {doc_id} failed: {:?}", resp.errors);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
