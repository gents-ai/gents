use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::{decode_merged, read_owned_doc, AgentAnchor};
use gents_documents::patch::SelfConfigTarget;
use gents_documents::Tools;
use gents_store::config_client::ConfigApplyTxn;

/// Lean `SelfConfig.keepsReach`.
pub fn guard_agent_keeps_reach(
    _stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    anyhow::ensure!(
        candidate.get("enabled").and_then(Value::as_bool) != Some(false),
        "no-lockout guard: agent must remain enabled"
    );
    Ok(())
}

/// Lean `SelfConfig.keepsControl`; operator grants are bounded separately by
/// [`super::guard_tools_keep_grants`].
pub fn guard_tools_keep_control(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let control = |tools: Tools| {
        let config = tools.self_config.unwrap_or_default();
        [
            config.enable_self_config.unwrap_or(false),
            tools
                .agents
                .and_then(|agents| agents.enabled)
                .unwrap_or(false),
            config.self_config_no_lockout.unwrap_or(false),
            config
                .self_config_categories
                .as_ref()
                .is_none_or(|categories| {
                    categories.iter().any(|category| category.trim() == "tools")
                }),
        ]
    };
    let [_, had_agents, had_guard, had_tools] = control(decode_merged("Tools", stored)?);
    let [self_config, agents, guard, tools] = control(decode_merged("Tools", candidate)?);
    anyhow::ensure!(
        self_config,
        "no-lockout guard: self-config must remain enabled in Tools.self_config.enable_self_config. Read tools get and preserve that group when updating Tools or selecting a replacement Context"
    );
    anyhow::ensure!(
        !had_agents || agents,
        "no-lockout guard: the agents tools must remain enabled"
    );
    anyhow::ensure!(
        !had_guard || guard,
        "no-lockout guard: self_config_no_lockout must remain enabled"
    );
    anyhow::ensure!(
        !had_tools || tools,
        "no-lockout guard: self-config must keep the tools category"
    );
    Ok(())
}

pub(crate) async fn guard_compaction_choice_in_txn(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    anchor: &AgentAnchor,
    stored: &Map<String, Value>,
    merged: &Map<String, Value>,
) -> Result<()> {
    let field = |doc: &Map<String, Value>, name: &str| {
        doc.get(name).and_then(Value::as_str).map(ToOwned::to_owned)
    };
    let owner = field(&anchor.doc, "node_did").context("agent is missing node_did")?;
    let read = |collection: SelfConfigTarget, id: Option<String>, name: &'static str| {
        let owner = owner.clone();
        async move {
            Ok::<_, anyhow::Error>(match id {
                Some(id) => read_owned_doc(txn, collection, &owner, &id)
                    .await?
                    .and_then(|(_, doc)| field(&doc, name)),
                None => None,
            })
        }
    };
    let (current, next) = match target {
        SelfConfigTarget::AgentContext => {
            let profile = field(&anchor.doc, "inference_profile_id");
            (
                (field(stored, "compaction_id"), profile.clone()),
                (field(merged, "compaction_id"), profile),
            )
        }
        SelfConfigTarget::Agent => {
            let context = |doc: &Map<String, Value>| {
                read(
                    SelfConfigTarget::AgentContext,
                    field(doc, "context_id"),
                    "compaction_id",
                )
            };
            (
                (
                    context(stored).await?,
                    field(stored, "inference_profile_id"),
                ),
                (
                    context(merged).await?,
                    field(merged, "inference_profile_id"),
                ),
            )
        }
        _ => return Ok(()),
    };
    if current == next {
        return Ok(());
    }
    let backend = |(compaction, profile): (Option<String>, Option<String>)| async {
        let profile = read(
            SelfConfigTarget::Compaction,
            compaction,
            "inference_profile_id",
        )
        .await?
        .or(profile)
        .context("agent inference profile is missing")?;
        profile_backend_id(txn, &owner, &profile).await
    };
    let (current, next) = (backend(current).await?, backend(next).await?);
    gents_inference::operations::admit_backend_choice_in_txn(txn, &owner, Some(&current), &next)
        .await?;
    Ok(())
}
pub async fn profile_backend_id(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    profile_id: &str,
) -> Result<String> {
    read_owned_doc(txn, SelfConfigTarget::InferenceProfile, owner, profile_id)
        .await?
        .and_then(|(_, doc)| doc.get("backend_id")?.as_str().map(ToOwned::to_owned))
        .with_context(|| format!("InferenceProfile {profile_id:?} not found"))
}
