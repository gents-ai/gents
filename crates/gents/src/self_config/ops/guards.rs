use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use super::{decode_merged, read_owned_doc, AgentAnchor};
use crate::config_client::patch::SelfConfigTarget;
use crate::config_client::ConfigApplyTxn;
use crate::document_config::{BackendAuth, Tools};
use crate::inference::backends::provider::BackendProviderOauthExt;

/// Lean `SelfConfig.keepsReach`.
pub fn guard_agent_keeps_reach(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let engineer_tag = |doc: &Map<String, Value>| {
        doc.get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| {
                tags.iter()
                    .any(|tag| tag.as_str() == Some(crate::self_config::ENGINEER_AGENT_TAG))
            })
    };
    anyhow::ensure!(
        candidate.get("enabled").and_then(Value::as_bool) != Some(false),
        "no-lockout guard: agent must remain enabled"
    );
    anyhow::ensure!(
        !engineer_tag(stored) || engineer_tag(candidate),
        "no-lockout guard: the Engineer tag must remain on the configurator"
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

pub fn guard_backend_auth(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let auth = |doc: &Map<String, Value>| {
        serde_json::from_value::<BackendAuth>(doc.get("auth").cloned().unwrap_or(Value::Null))
            .map_err(|error| anyhow!("backend auth is not valid: {error}"))
    };
    let (stored, candidate) = (auth(stored)?, auth(candidate)?);
    if matches!(candidate, BackendAuth::ApiKey { .. }) {
        anyhow::ensure!(
            candidate == stored,
            "raw API keys are operator-managed; select an environment or OAuth reference"
        );
    }
    if let BackendAuth::NodeOAuth { account_ref } = &candidate {
        let stored_ref = match &stored {
            BackendAuth::NodeOAuth { account_ref } => account_ref.as_ref(),
            _ => None,
        };
        anyhow::ensure!(
            account_ref.as_ref() == stored_ref,
            "OAuth account references are operator-managed; keep the backend's account_ref as stored"
        );
    }
    Ok(())
}
/// Lean `SelfConfig.backendChoiceAllowed`; `default_account` is `next`'s
/// provider default account (Lean `dflt`).
pub fn guard_backend_choice(
    current: Option<&crate::InferenceBackend>,
    next: &crate::InferenceBackend,
    default_account: Option<&str>,
) -> Result<()> {
    let BackendAuth::NodeOAuth { account_ref } = &next.auth else {
        return Ok(());
    };
    let allowed = match current {
        Some(current) if current.provider_kind == next.provider_kind => current.auth == next.auth,
        _ => account_ref.as_deref() == default_account,
    };
    anyhow::ensure!(
        allowed,
        "backend {} selects another {} account; keep the current account, or pick an account-free backend or another provider's default account",
        next.backend_id,
        next.provider_kind
    );
    Ok(())
}
pub(crate) async fn guard_backend_choice_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    current_backend_id: Option<&str>,
    next_backend_id: &str,
) -> Result<()> {
    let backend = |id: String| async move {
        read_owned_doc(txn, SelfConfigTarget::InferenceBackend, owner, &id)
            .await?
            .map(|(_, doc)| crate::InferenceBackend::from_value(&Value::Object(doc)))
            .transpose()
    };
    let current = match current_backend_id {
        Some(id) => backend(id.to_owned()).await?,
        None => None,
    };
    let next = backend(next_backend_id.to_owned())
        .await?
        .with_context(|| format!("InferenceBackend {next_backend_id:?} not found"))?;
    let default_account = match (&next.auth, next.provider_kind.oauth_provider()) {
        (BackendAuth::NodeOAuth { .. }, Some(provider)) => {
            crate::inference::auth::credential::provider_default_account_ref(txn, owner, provider)
                .await?
        }
        _ => None,
    };
    guard_backend_choice(current.as_ref(), &next, default_account.as_deref())
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
    guard_backend_choice_in_txn(txn, &owner, Some(&current), &next).await
}
pub(crate) async fn profile_backend_id(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    profile_id: &str,
) -> Result<String> {
    read_owned_doc(txn, SelfConfigTarget::InferenceProfile, owner, profile_id)
        .await?
        .and_then(|(_, doc)| doc.get("backend_id")?.as_str().map(ToOwned::to_owned))
        .with_context(|| format!("InferenceProfile {profile_id:?} not found"))
}
