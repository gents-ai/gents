use super::*;
use gents_session::session::output_render::Ordered;

/// The Node's default Agent. With a runtime Node binding it is refreshed as
/// the runtime reads it; otherwise it is the selection document as the
/// invoking Agent reads it.
pub async fn node_default_agent(core: &SelfConfigCore) -> Result<Option<String>> {
    if core.node_binding().is_some() {
        return Ok(core
            .refresh_node_binding()
            .await?
            .context("runtime Node binding is unavailable")?
            .default_agent_did);
    }
    let node_did = core.node_did();
    core.access()?
        .transact_readonly("self_config.default_agent", |txn| {
            Box::pin(gents_documents::load_default_agent_did_in_txn(
                txn, node_did,
            ))
        })
        .await
}

async fn agent_snapshot(
    core: &SelfConfigCore,
    agent_did: &str,
    default_agent_did: Option<&str>,
) -> Result<Value> {
    let effective = SelfConfigCore::new(
        core.node_handle(),
        core.node_did().to_owned(),
        agent_did.to_owned(),
    )?
    .with_lockout_agent_did(core.identity()?.to_string())
    .inherit_invoker_capabilities(core)
    .with_process_ceiling(core.process_ceiling().clone())
    .read_effective_config(&BTreeSet::new(), false, false)
    .await?;
    Ok(json!({
        "agent_did": agent_did,
        "is_default": default_agent_did == Some(agent_did),
        "effective_config": effective,
    }))
}

pub(super) async fn agent_list(
    core: &SelfConfigCore,
    limit: usize,
    cursor: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        (1..=50).contains(&limit),
        "--limit must be between 1 and 50"
    );
    let catalog = gents_inference::views::agents::read_agent_catalog(
        &core.access()?,
        core.node_did(),
        cursor,
        limit,
    )
    .await?;
    let default_agent_did = catalog.default_agent_did;
    let agents = catalog
        .agents
        .iter()
        .map(|row| {
            let agent = &row.agent.agent;
            let profile = row.profile.as_ref().map(|row| &row.profile);
            json!({
                "agent_did": agent.agent_did,
                "display_name": agent.display_name,
                "description": agent.description,
                "enabled": agent.enabled,
                "is_default": default_agent_did.as_deref() == Some(agent.agent_did.as_str()),
                "context_id": agent.context_id,
                "profile_id": agent.inference_profile_id,
                "backend_id": profile.map(|profile| &profile.backend_id),
                "model_name": profile.map(|profile| &profile.model_name),
                "reasoning_effort": profile.and_then(|profile| profile.reasoning_effort.as_ref()),
                "service_tier": profile.and_then(|profile| profile.service_tier.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    let snapshot = json!({
        "page": {
            "limit": limit,
            "total": catalog.total,
            "returned": agents.len(),
            "truncated": catalog.truncated,
            "next_cursor": catalog.next_cursor,
        },
        "process_ceiling": core.process_ceiling(),
        "permission_presets": builtin_preset_names(),
        "available_profile_ids": catalog.profile_ids,
        "default_agent_did": default_agent_did,
        "agents": agents,
    });
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

pub async fn agent_inspect(core: &SelfConfigCore, agent_did: &str) -> Result<String> {
    let catalog = load_agent_catalog(core).await?;
    let _ = catalog.agents.get(agent_did).with_context(|| {
        format!("unknown agent_did {agent_did:?}; copy an exact ID from [\"agent\",\"list\"]")
    })?;
    let default_agent_did = node_default_agent(core).await?;
    let snapshot = agent_snapshot(core, agent_did, default_agent_did.as_deref()).await?;
    let Value::Object(mut snapshot) = snapshot else {
        bail!("agent snapshot must be an object");
    };
    let effective = snapshot.remove("effective_config").unwrap_or_default();
    let mut result = Ordered::reading_order(Value::Object(snapshot), &["agent_did", "is_default"]);
    result.push(Ordered::entry(
        "effective_config",
        &Ordered::reading_order(effective, EFFECTIVE_ORDER),
    ));
    result.pretty()
}

pub(super) async fn guard_agent_profile_choice(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    current_agent: Option<&str>,
    clone_from: Option<&str>,
    next_profile: Option<&str>,
) -> Result<()> {
    let Some(next_profile) = next_profile else {
        return Ok(());
    };
    let current_profile = match current_agent {
        Some(agent_did) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, agent_did)
            .await?
            .and_then(|(_, doc)| {
                doc.get("inference_profile_id")?
                    .as_str()
                    .map(ToOwned::to_owned)
            }),
        None => None,
    };
    let current_backend = match current_profile {
        Some(profile) => Some(ops::profile_backend_id(txn, node_did, &profile).await?),
        None => None,
    };
    let next_backend = ops::profile_backend_id(txn, node_did, next_profile).await?;
    gents_inference::operations::admit_backend_choice_in_txn(
        txn,
        node_did,
        current_backend.as_deref(),
        &next_backend,
    )
    .await?;
    let source = match clone_from {
        Some(id) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, id).await?,
        None => None,
    };
    let Some((_, source)) = source else {
        return Ok(());
    };
    let own =
        serde_json::Map::from_iter([("inference_profile_id".to_owned(), json!(next_profile))]);
    let mut cloned = own.clone();
    if let Some(context) = source.get("context_id") {
        cloned.insert("context_id".to_owned(), context.clone());
    }
    let anchor = ops::AgentAnchor {
        doc: source,
        context: serde_json::Map::new(),
        profile: serde_json::Map::new(),
        execution: serde_json::Map::new(),
    };
    ops::guard_compaction_choice_in_txn(txn, SelfConfigTarget::Agent, &anchor, &own, &cloned).await
}
