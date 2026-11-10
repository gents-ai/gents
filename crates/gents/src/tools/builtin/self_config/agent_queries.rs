use super::*;

/// The runtime refreshes its physical Node selection without changing the
/// invoking Agent used for Agent reads and writes.
pub(super) async fn node_default_agent(core: &SelfConfigCore) -> Result<Option<String>> {
    if core.node_binding().is_some() {
        return Ok(core
            .refresh_node_binding()
            .await?
            .context("runtime Node binding is unavailable")?
            .default_agent_did);
    }
    let query = format!(
        r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{ default_agent_did }} }}"#,
        escape_graphql_string(core.node_did())
    );
    let access = core.access()?;
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("Node"))
        .and_then(Value::as_array)
        .context("Node query returned no rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous node identity");
    Ok(rows
        .first()
        .and_then(|row| row.get("default_agent_did"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn agent_snapshot(
    core: &SelfConfigCore,
    agent_did: &str,
    protected: bool,
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
        "protected": protected,
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
    let catalog = load_agent_catalog(core).await?;
    let default_agent_did = node_default_agent(core).await?;
    let total = catalog.agents.len();
    let selected = catalog
        .agents
        .iter()
        .filter(|(agent_did, _)| cursor.is_none_or(|cursor| agent_did.as_str() > cursor))
        .take(limit + 1)
        .collect::<Vec<_>>();
    let truncated = selected.len() > limit;
    let mut agents = Vec::new();
    for (agent_did, reference) in selected.into_iter().take(limit) {
        let snapshot = match agent_snapshot(
            core,
            agent_did,
            catalog.protected_ids.contains(agent_did),
            default_agent_did.as_deref(),
        )
        .await
        {
            Ok(snapshot) => {
                let effective = &snapshot["effective_config"];
                json!({
                    "agent_did": agent_did,
                    "display_name": effective.pointer("/agent/display_name"),
                    "description": effective.pointer("/agent/description"),
                    "enabled": *reference,
                    "protected": catalog.protected_ids.contains(agent_did),
                    "is_default": default_agent_did.as_deref() == Some(agent_did.as_str()),
                    "context_id": effective.pointer("/agent/context_id"),
                    "profile_id": effective.pointer("/agent/inference_profile_id"),
                    "backend_id": effective.pointer("/inference_profile/backend_id"),
                    "model_name": effective.pointer("/inference_profile/model_name"),
                    "reasoning_effort": effective.pointer("/inference_profile/reasoning_effort"),
                    "service_tier": effective.pointer("/inference_profile/service_tier"),
                })
            }
            Err(error) => json!({
                "agent_did": agent_did,
                "enabled": *reference,
                "protected": catalog.protected_ids.contains(agent_did),
                "is_default": default_agent_did.as_deref() == Some(agent_did.as_str()),
                "configuration_error": format!("{error:#}"),
            }),
        };
        agents.push(snapshot);
    }
    let next_cursor = truncated
        .then(|| {
            agents
                .last()?
                .get("agent_did")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .flatten();
    let snapshot = json!({
        "page": {
            "limit": limit,
            "total": total,
            "returned": agents.len(),
            "truncated": truncated,
            "next_cursor": next_cursor,
        },
        "process_ceiling": core.process_ceiling(),
        "permission_presets": builtin_preset_names(),
        "available_profile_ids": catalog.published_profiles,
        "default_agent_did": default_agent_did,
        "agents": agents,
        "activation": {
            "durable_config": "after the admitted transaction commits",
            "running_generation": "after the runtime reconciler generation swap",
            "session": "select the agent in a new session; existing sessions remain bound to their original agent",
            "pairing_and_restart": "canonical documents replicate to authorized paired clients and are reloaded after restart",
        },
    });
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

pub(super) async fn agent_inspect(core: &SelfConfigCore, agent_did: &str) -> Result<String> {
    let catalog = load_agent_catalog(core).await?;
    let _ = catalog.agents.get(agent_did).with_context(|| {
        format!("unknown agent_did {agent_did:?}; copy an exact ID from [\"agent\",\"list\"]")
    })?;
    let default_agent_did = node_default_agent(core).await?;
    let snapshot = agent_snapshot(
        core,
        agent_did,
        catalog.protected_ids.contains(agent_did),
        default_agent_did.as_deref(),
    )
    .await?;
    let Value::Object(mut snapshot) = snapshot else {
        bail!("agent snapshot must be an object");
    };
    let effective = snapshot.remove("effective_config").unwrap_or_default();
    let mut result = Ordered::reading_order(
        Value::Object(snapshot),
        &["agent_did", "is_default", "protected"],
    );
    result.push(Ordered::entry(
        "effective_config",
        &Ordered::reading_order(effective, EFFECTIVE_ORDER),
    ));
    result.pretty()
}

pub(super) async fn guard_agent_profile_choice(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
    ops::guard_backend_choice_in_txn(txn, node_did, current_backend.as_deref(), &next_backend)
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
